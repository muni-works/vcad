import SwiftUI
import RealityKit
import AppKit
import CVcadFFI

// Live racing-plasmoid demo (docs/plasmoid-sim.md, gap #2): open
// docs/plasmoid-sim/plasmoid-dish.vcad, turn on Analyze ▸ Live Plasmoid, and
// drag the `magnet` part over the dish — the plasmoid's foot locks under it
// when it is close and slow, breaks away on a fast flick, and is recaptured
// on the next pass. Raise the pressure toward 1 atm and the lock vanishes.
//
// The physics is the kernel's reduced model (vcad_kernel_particle::plasmoid
// via `vcad_plasmoid_*`): every display frame hands the magnet's current
// pose to the kernel, which integrates fixed RK4 substeps (≈ 1/2400 s) with
// the pose interpolated across the frame. What is drawn on top — the
// filament arc from the spike to the foot, its flicker, the halo — is
// stylization; vcad has no volumetric glow or bloom yet (gap #3).
//
// Geometry comes from the document, not constants: the parts named `dish`,
// `spike` and `magnet` (loon binding names, see plasmoid-dish.loon). Dish
// centre and rim plane, the dish radius (bbox half-width, so it includes
// the 2 mm wall) and the spike tip are their bounding boxes; the magnet disc
// is the bottom 5 mm of the `magnet` part. Still assumed, not read: the
// magnet's grade/size (N52 10 × 5 mm) and its polarity (north down — a
// rotated magnet part is not reflected).
//
// VcadApp-only on purpose (like FieldOverlay.swift): Editor.swift and
// Shell.swift are symlinked into VcadVision, so nothing here hangs off
// EditorModel.

/// An axis-aligned box in kernel mm (a part's bounds).
struct PlasmoidBox: Equatable {
    var lo: SIMD3<Double>
    var hi: SIMD3<Double>
    var center: SIMD3<Double> { (lo + hi) / 2 }
    init(lo: SIMD3<Double>, hi: SIMD3<Double>) { self.lo = lo; self.hi = hi }
    init(_ m: EditorModel.PickMesh) { lo = SIMD3<Double>(m.lo); hi = SIMD3<Double>(m.hi) }
}

/// Which document parts the demo is bound to, and the frame math that turns
/// their bounds into the kernel's dish-frame inputs. Pure: unit-tested.
struct PlasmoidBinding: Equatable {
    var dishPart: Int
    /// Every part named `magnet…` (`magnet`, `magnet-2`, `magnet_b`, …), in
    /// part order — all superposed in one kernel field scene.
    var magnetParts: [Int]
    var spikePart: Int?
    /// The first magnet (the one the demo selects and drags).
    var magnetPart: Int { magnetParts[0] }

    /// Thickness of the disc at the bottom of the `magnet` part, mm.
    static let magnetThicknessMM = 5.0

    /// Bind by part name (case-insensitive): `dish` and at least one
    /// `magnet…` required.
    static func find(names: [String]) -> PlasmoidBinding? {
        let lower = names.map { $0.lowercased() }
        let magnets = lower.indices.filter { lower[$0].hasPrefix("magnet") }
        guard let d = lower.firstIndex(of: "dish"), !magnets.isEmpty else { return nil }
        return PlasmoidBinding(dishPart: d, magnetParts: magnets, spikePart: lower.firstIndex(of: "spike"))
    }

    /// Dish centre with z on the rim plane (the sim's z = 0), kernel mm.
    static func dishOrigin(_ dish: PlasmoidBox) -> SIMD3<Double> {
        SIMD3(dish.center.x, dish.center.y, dish.hi.z)
    }

    static func dishRadius(_ dish: PlasmoidBox) -> Double {
        0.5 * max(dish.hi.x - dish.lo.x, dish.hi.y - dish.lo.y)
    }

    /// Where the filament roots: the spike's tip, else the dish centre.
    static func spikeTip(_ spike: PlasmoidBox?, dish: PlasmoidBox) -> SIMD3<Double> {
        guard let s = spike else { return dishOrigin(dish) }
        return SIMD3(s.center.x, s.center.y, s.hi.z)
    }

    /// The magnet disc's centre in kernel mm. `liveCenter` is the part's
    /// bbox centre mid-drag (grab centre + gizmo offset), used instead of the
    /// bounds while a drag's re-eval has not landed yet.
    static func magnetCenter(_ magnet: PlasmoidBox, liveCenter: SIMD3<Double>?) -> SIMD3<Double> {
        let c = liveCenter ?? magnet.center
        let bottom = c.z - (magnet.hi.z - magnet.lo.z) / 2
        return SIMD3(c.x, c.y, bottom + magnetThicknessMM / 2)
    }

    /// Kernel pose for `vcad_plasmoid_advance`: disc centre in the dish
    /// frame + magnetization axis (north down).
    static func kernelPose(magnetCenter: SIMD3<Double>, dishOrigin: SIMD3<Double>) -> [Double] {
        let p = magnetCenter - dishOrigin
        return [p.x, p.y, p.z, 0, 0, -1]
    }
}

/// What the HUD and the overlay read after each frame.
struct PlasmoidReadout: Equatable {
    var t: Double = 0
    /// Foot in kernel mm (on the dish surface when the mesh is known).
    var foot: SIMD3<Double> = .zero
    /// Filament root (spike tip), kernel mm.
    var root: SIMD3<Double> = .zero
    var beta: Double = 0
    var locked = false
    /// Which of the bound magnets holds the foot (index into `magnetParts`).
    var lockedMagnet: Int?
    var magnetCount = 0
    var hasMagnet: Bool { magnetCount > 0 }
    /// Kernel substeps run last frame.
    var substeps = 0
}

/// Everything one sim frame needs, captured on the main actor and handed to
/// the kernel queue (value types only).
struct PlasmoidFrameInput {
    var dt: Double
    var dish: PlasmoidBox
    var spike: PlasmoidBox?
    /// Disc centres of every magnet (kernel mm); empty = no magnet.
    var magnetCenters: [SIMD3<Double>]
    var pressureTorr: Double
    var reset = false
    var dishMesh: EditorModel.PickMesh?
}

/// Owns the `VcadPlasmoid` handle. Not `Sendable` in any real sense: in the
/// live loop it is touched only on `PlasmoidStore.queue` (one step in flight
/// at a time); in tests and the scripted capture only synchronously on the
/// main actor, never both.
final class PlasmoidKernel: @unchecked Sendable {
    private var handle: OpaquePointer?
    private var radius = 0.0
    private var pressure = 1.0

    deinit { if let h = handle { vcad_plasmoid_free(h) } }

    func free() {
        if let h = handle { vcad_plasmoid_free(h) }
        handle = nil
    }

    /// One frame: (re)create for the dish, apply pressure / reset, advance.
    func run(_ inp: PlasmoidFrameInput) -> (PlasmoidReadout?, String?) {
        let r = PlasmoidBinding.dishRadius(inp.dish)
        if handle == nil || abs(r - radius) > 0.5 {
            free()
            handle = vcad_plasmoid_create(r, inp.pressureTorr)
            radius = r; pressure = inp.pressureTorr
            guard handle != nil else { return (nil, "plasmoid: \(Self.lastError() ?? "create failed")") }
        }
        guard let h = handle else { return (nil, nil) }
        if inp.pressureTorr != pressure { vcad_plasmoid_set_pressure(h, inp.pressureTorr); pressure = inp.pressureTorr }
        if inp.reset { vcad_plasmoid_reset(h) }
        let origin = PlasmoidBinding.dishOrigin(inp.dish)
        let n = max(1, Int((inp.dt / PlasmoidStore.targetDt).rounded(.up)))
        let poses = inp.magnetCenters.flatMap { PlasmoidBinding.kernelPose(magnetCenter: $0, dishOrigin: origin) }
        let s = poses.withUnsafeBufferPointer {
            vcad_plasmoid_advance_multi(h, poses.isEmpty ? nil : $0.baseAddress, inp.magnetCenters.count, inp.dt, UInt32(n))
        }
        let fx = origin.x + s.x_mm, fy = origin.y + s.y_mm
        let fz = inp.dishMesh.flatMap { PlasmoidStore.surfaceZ(x: Float(fx), y: Float(fy), mesh: $0) }
            .map(Double.init) ?? origin.z
        return (PlasmoidReadout(t: s.t_s, foot: SIMD3(fx, fy, fz),
                                root: PlasmoidBinding.spikeTip(inp.spike, dish: inp.dish),
                                beta: s.beta, locked: s.locked != 0,
                                lockedMagnet: s.locked_magnet >= 0 ? Int(s.locked_magnet) : nil,
                                magnetCount: inp.magnetCenters.count, substeps: n), nil)
    }

    private static func lastError() -> String? {
        var len = 0
        guard let p = vcad_last_error(&len) else { return nil }
        return String(decoding: UnsafeBufferPointer(start: p, count: len), as: UTF8.self)
    }
}

@MainActor @Observable
final class PlasmoidStore {
    static let shared = PlasmoidStore()

    /// Integrator step target, s (the doc's demo run: RK4 at 2.4 kHz).
    nonisolated static let targetDt = 1.0 / 2400.0
    /// Longest frame the sim will integrate; a stall slows sim time instead
    /// of teleporting the magnet (or spiralling: more substeps → slower frame).
    static let maxFrame = 1.0 / 20.0

    var active = false
    /// Gas pressure as log10(Torr) — what the HUD slider binds to directly.
    var pressureLog10 = 0.0
    var pressureTorr: Double { pow(10, pressureLog10) }
    var readout = PlasmoidReadout()
    var error: String?
    /// Bumped every sim frame; a `ReleasedARView` input so the overlay redraws.
    var tick = 0
    /// Recent foot positions (kernel mm), newest last — the comet tail.
    @ObservationIgnored var trail: [SIMD3<Double>] = []

    /// The kernel runs here in the live loop, never on the main thread: a
    /// busy UI frame must not slow the plasmoid down, and the sim must not
    /// starve the UI (or the document's own part streaming).
    @ObservationIgnored private let queue = DispatchQueue(label: "io.vcad.plasmoid", qos: .userInitiated)
    @ObservationIgnored private var kernel = PlasmoidKernel()
    @ObservationIgnored private var inFlight = false
    @ObservationIgnored private var pendingReset = false
    @ObservationIgnored private(set) var binding: PlasmoidBinding?
    @ObservationIgnored private var boundPartCount = -1
    @ObservationIgnored private var introduced = false
    @ObservationIgnored private var timer: Timer?
    @ObservationIgnored private var lastFrame: CFTimeInterval = 0
    @ObservationIgnored weak var model: EditorModel?
    /// When set, every frame advances by exactly this much sim time,
    /// synchronously (the scripted capture), instead of wall-clock time.
    @ObservationIgnored var fixedFrameDt: Double?

    func attach(_ m: EditorModel) { model = m }

    func toggle() { active ? stop() : start() }

    func start() {
        guard let m = model else { error = "no document window"; return }
        active = true
        error = nil
        readout = PlasmoidReadout()
        trail = []
        lastFrame = CACurrentMediaTime()
        boundPartCount = -1
        introduced = false
        bindIfNeeded(m)
        // The loop runs even before the parts are bound: a document still
        // streaming its parts in binds on a later frame.
        if fixedFrameDt == nil {
            let t = Timer(timeInterval: 1.0 / 60.0, repeats: true) { _ in
                MainActor.assumeIsolated { PlasmoidStore.shared.frame() }
            }
            // `.common` so the sim keeps running inside a drag's tracking loop.
            RunLoop.main.add(t, forMode: .common)
            timer = t
        }
        tick += 1
    }

    /// (Re)bind when the part count changes; on the first successful bind,
    /// select the magnet (its move gizmo up, ready to drag) and rebuild the
    /// scene once — a part that finished evaluating after the first scene
    /// build can be missing until the next refresh (seen with this
    /// document's `magnet` on open, a pre-existing load race).
    private func bindIfNeeded(_ m: EditorModel) {
        guard m.docPartMeshes.count != boundPartCount else { return }
        bind(m)
        guard let b = binding else {
            error = "needs parts named “dish” and “magnet” — open docs/plasmoid-sim/plasmoid-dish.vcad"
            return
        }
        error = nil
        if !introduced {
            introduced = true
            m.selectParts([b.magnetPart])
            ReleaseWindowController.shared.refreshGeometry(model: m, collisions: true)
        }
    }

    func stop() {
        timer?.invalidate(); timer = nil
        active = false
        error = nil
        // A fresh kernel; the old one is freed on the queue after any step in flight.
        let old = kernel
        kernel = PlasmoidKernel()
        queue.async { old.free() }
        inFlight = false
        tick += 1
    }

    func reset() { pendingReset = true; trail = [] }

    func setPressure(_ torr: Double) { pressureLog10 = log10(max(torr, 1e-3)) }

    private func bind(_ m: EditorModel) {
        let n = m.docPartMeshes.count
        binding = PlasmoidBinding.find(names: (0..<n).map { m.partDisplayName($0) })
        boundPartCount = n
    }

    /// One display frame from the live model: read the magnet, step, publish.
    func frame() {
        guard active, let m = model else { return }
        bindIfNeeded(m)
        guard let b = binding, b.dishPart < m.docPartMeshes.count,
              b.magnetParts.allSatisfy({ $0 < m.docPartMeshes.count }) else { return }
        // One step in flight at most: a slow step drops display frames
        // instead of queueing a backlog.
        if fixedFrameDt == nil, inFlight { return }
        let now = CACurrentMediaTime()
        let dt = fixedFrameDt ?? min(Self.maxFrame, max(0, now - lastFrame))
        lastFrame = now
        let meshes = m.docPartMeshes
        // The dragged magnet (whichever it is) reads the gizmo's live
        // centre; the rest read their bounds.
        let drag = m.gizmoTranslateDrag
        let centers = b.magnetParts.map { part in
            PlasmoidBinding.magnetCenter(PlasmoidBox(meshes[part]),
                                         liveCenter: drag?.part == part ? drag.map { SIMD3<Double>($0.center) } : nil)
        }
        let inp = PlasmoidFrameInput(
            dt: dt, dish: PlasmoidBox(meshes[b.dishPart]),
            spike: b.spikePart.flatMap { $0 < meshes.count ? PlasmoidBox(meshes[$0]) : nil },
            magnetCenters: centers,
            pressureTorr: pressureTorr, reset: pendingReset, dishMesh: meshes[b.dishPart])
        pendingReset = false
        if fixedFrameDt != nil {
            publish(kernel.run(inp))
        } else {
            inFlight = true
            let k = kernel
            queue.async {
                let out = k.run(inp)
                DispatchQueue.main.async {
                    MainActor.assumeIsolated {
                        let s = PlasmoidStore.shared
                        s.inFlight = false
                        if s.active, s.kernel === k { s.publish(out) }
                    }
                }
            }
        }
        RenderGate.wake()
    }

    /// Advance the kernel by `dt` with the magnet disc at `magnetCenter`
    /// (kernel mm; nil = no magnet) synchronously and publish the readout —
    /// the same `PlasmoidKernel.run` the live loop runs on its queue.
    func step(dt: Double, dish: PlasmoidBox, spike: PlasmoidBox?, magnetCenter: SIMD3<Double>?) {
        step(dt: dt, dish: dish, spike: spike, magnetCenters: magnetCenter.map { [$0] } ?? [])
    }

    /// `step` with any number of magnets superposed.
    func step(dt: Double, dish: PlasmoidBox, spike: PlasmoidBox?, magnetCenters: [SIMD3<Double>]) {
        let inp = PlasmoidFrameInput(dt: dt, dish: dish, spike: spike, magnetCenters: magnetCenters,
                                     pressureTorr: pressureTorr, reset: pendingReset)
        pendingReset = false
        publish(kernel.run(inp))
    }

    private func publish(_ out: (PlasmoidReadout?, String?)) {
        if let e = out.1 { error = e }
        guard let r = out.0 else { return }
        readout = r
        trail.append(r.foot)
        if trail.count > 18 { trail.removeFirst(trail.count - 18) }
        tick += 1
    }

    /// Highest point of `mesh` straight below/above (x, y) — the bowl
    /// surface under the foot. Brute force over the triangles: the dish is a
    /// few thousand, once a frame.
    nonisolated static func surfaceZ(x: Float, y: Float, mesh: EditorModel.PickMesh) -> Float? {
        guard x >= mesh.lo.x, x <= mesh.hi.x, y >= mesh.lo.y, y <= mesh.hi.y else { return nil }
        var best: Float?
        let p = mesh.positions, idx = mesh.indices
        var i = 0
        while i + 2 < idx.count {
            let a = p[Int(idx[i])], b = p[Int(idx[i + 1])], c = p[Int(idx[i + 2])]
            i += 3
            let d = (b.y - c.y) * (a.x - c.x) + (c.x - b.x) * (a.y - c.y)
            if abs(d) < 1e-9 { continue }
            let w1 = ((b.y - c.y) * (x - c.x) + (c.x - b.x) * (y - c.y)) / d
            let w2 = ((c.y - a.y) * (x - c.x) + (a.x - c.x) * (y - c.y)) / d
            let w3 = 1 - w1 - w2
            guard w1 >= 0, w2 >= 0, w3 >= 0 else { continue }
            let z = w1 * a.z + w2 * b.z + w3 * c.z
            if best == nil || z > best! { best = z }
        }
        return best
    }
}

// MARK: - Viewport overlay

private enum PlasmoidLook {
    static let segments = 14
    static let trailCount = 18
    @MainActor static let core = UnlitMaterial(color: NSColor(red: 1, green: 0.93, blue: 0.78, alpha: 1))
    @MainActor static let filament = UnlitMaterial(color: NSColor(red: 1, green: 0.62, blue: 0.22, alpha: 1))
    @MainActor static func glow(_ a: Float) -> UnlitMaterial {
        var m = UnlitMaterial(color: NSColor(red: 1, green: 0.5, blue: 0.12, alpha: 1))
        m.blending = .transparent(opacity: .init(scale: a))
        return m
    }
}

/// Build (once) and re-pose (every tick) the plasmoid entities under
/// `parent`, the kernel-mm frame. Fixed meshes, transforms only — no mesh
/// regeneration per frame.
@MainActor
func syncPlasmoidOverlay(_ store: PlasmoidStore, in parent: Entity) {
    let root: Entity
    if let existing = parent.findEntity(named: "plasmoidRoot") { root = existing }
    else {
        root = Entity(); root.name = "plasmoidRoot"
        let foot = ModelEntity(mesh: .generateSphere(radius: 1.8), materials: [PlasmoidLook.core])
        foot.name = "pmFoot"; root.addChild(foot)
        let halo = ModelEntity(mesh: .generateSphere(radius: 5.5), materials: [PlasmoidLook.glow(0.28)])
        halo.name = "pmHalo"; root.addChild(halo)
        let seg = MeshResource.generateCylinder(height: 1, radius: 0.55)
        let sheath = MeshResource.generateCylinder(height: 1, radius: 1.6)
        for i in 0..<PlasmoidLook.segments {
            let e = ModelEntity(mesh: seg, materials: [PlasmoidLook.filament]); e.name = "pmSeg\(i)"; root.addChild(e)
            let g = ModelEntity(mesh: sheath, materials: [PlasmoidLook.glow(0.22)]); g.name = "pmSheath\(i)"; root.addChild(g)
        }
        let dot = MeshResource.generateSphere(radius: 1)
        for i in 0..<PlasmoidLook.trailCount {
            let t = ModelEntity(mesh: dot, materials: [PlasmoidLook.glow(0.08 + 0.3 * Float(i) / Float(PlasmoidLook.trailCount))])
            t.name = "pmTrail\(i)"; root.addChild(t)
        }
        parent.addChild(root)
    }
    root.isEnabled = store.active && store.binding != nil && store.tick > 0 && store.readout.substeps > 0
    guard root.isEnabled else { return }
    let r = store.readout
    let foot = SIMD3<Float>(r.foot) + SIMD3(0, 0, 1.2)
    let tip = SIMD3<Float>(r.root)
    root.findEntity(named: "pmFoot")?.position = foot
    if let halo = root.findEntity(named: "pmHalo") {
        halo.position = foot
        // Brighter, tighter foot when locked (the magnetized spot).
        let s: Float = r.locked ? 1.25 : 0.9
        halo.scale = SIMD3(s, s, 0.55 * s)
    }
    // Filament: a lifted quadratic arc from the spike tip to the foot, with a
    // deterministic per-tick flicker. Stylized — the model has no filament.
    let mid = (tip + foot) / 2
    let span = simd_length(foot - tip)
    let ctrl = mid + SIMD3(0, 0, 0.18 * span + 4)
    func hash(_ k: Int) -> Float {
        var x = UInt32(truncatingIfNeeded: k &* 2654435761 &+ store.tick &* 40503)
        x ^= x >> 13; x = x &* 0x5bd1e995; x ^= x >> 15
        return Float(x % 1000) / 1000 - 0.5
    }
    var pts: [SIMD3<Float>] = []
    for i in 0...PlasmoidLook.segments {
        let u = Float(i) / Float(PlasmoidLook.segments)
        let w0: Float = (1 - u) * (1 - u), w1: Float = 2 * (1 - u) * u, w2: Float = u * u
        var p: SIMD3<Float> = tip * w0
        p += ctrl * w1
        p += foot * w2
        let j: Float = sin(u * .pi) * 1.6
        p += SIMD3<Float>(hash(i * 3) * j, hash(i * 3 + 1) * j, hash(i * 3 + 2) * j * 0.6)
        pts.append(p)
    }
    for i in 0..<PlasmoidLook.segments {
        let a = pts[i], b = pts[i + 1]
        let d = b - a, len = max(simd_length(d), 1e-4)
        let q = simd_quatf(from: SIMD3(0, 1, 0), to: d / len)
        for name in ["pmSeg\(i)", "pmSheath\(i)"] {
            guard let e = root.findEntity(named: name) else { continue }
            e.position = (a + b) / 2
            e.orientation = q
            e.scale = SIMD3(1, len, 1)
        }
    }
    let trail = store.trail
    for i in 0..<PlasmoidLook.trailCount {
        guard let e = root.findEntity(named: "pmTrail\(i)") else { continue }
        let k = trail.count - PlasmoidLook.trailCount + i
        e.isEnabled = k >= 0
        if k >= 0 {
            e.position = SIMD3<Float>(trail[k]) + SIMD3(0, 0, 1)
            let s = 0.6 + 1.6 * Float(i) / Float(PlasmoidLook.trailCount)
            e.scale = SIMD3(s, s, s)
        }
    }
}

// MARK: - HUD

/// β under the foot, locked / racing, and the pressure that makes it vanish.
struct PlasmoidHud: View {
    @Bindable var store: PlasmoidStore

    private static let logMax = log10(760.0)

    var body: some View {
        if store.active {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text("Live plasmoid").bold()
                    Spacer()
                    Button { store.stop() } label: { Image(systemName: "xmark") }.buttonStyle(.plain)
                        .help("Stop the live plasmoid")
                }
                if let e = store.error {
                    Text(e).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                } else {
                    let r = store.readout
                    HStack(spacing: 8) {
                        Text(!r.locked ? "RACING" : r.magnetCount > 1 ? "LOCKED · \((r.lockedMagnet ?? 0) + 1)" : "LOCKED")
                            .font(.system(size: 11, weight: .bold, design: .monospaced))
                            .padding(.horizontal, 6).padding(.vertical, 2)
                            .background((r.locked ? Color.green : Color.orange).opacity(0.25), in: Capsule())
                            .accessibilityIdentifier("plasmoid-lock-state")
                        Text(String(format: "β = %.2f", r.beta))
                        Spacer()
                        Text(String(format: "t %.1f s", r.t)).foregroundStyle(.secondary)
                    }
                    HStack {
                        Text("p")
                        Slider(value: $store.pressureLog10, in: 0...Self.logMax)
                        Text(store.pressureTorr >= 700 ? "1 atm" : String(format: "%.1f Torr", store.pressureTorr))
                            .frame(width: 70, alignment: .trailing)
                    }
                    Text(r.beta < 1 && r.hasMagnet
                         ? "β < 1 under the foot: glow not magnetized, no lock"
                         : "Select the magnet and drag it (gizmo XY plane, or ⌥-drag). Slow: it follows. Flick: it breaks away.")
                        .foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    HStack {
                        Button("Reset") { store.reset() }
                        Spacer()
                        Text("reduced model · RK4 \(r.substeps)×/frame · glow stylized")
                            .foregroundStyle(.tertiary)
                    }
                }
            }
            .font(.system(size: 11, design: .monospaced))
            .padding(10)
            .frame(width: 320)
            .panelSurface()
            .accessibilityIdentifier("plasmoid-hud")
        }
    }
}

// MARK: - Scripted capture (VCAD_PLASMOID_SNAPSHOT_DIR)

/// `VCAD_PLASMOID_SNAPSHOT_DIR=/out vcad --vcad-open plasmoid-dish.vcad`
/// turns the live plasmoid on and drives the magnet through the real move
/// gizmo (`beginGizmoDrag` / `gizmoDragTo` with scripted rays) — hold until
/// the racer locks, a slow drag it follows, a flick it breaks away from,
/// then a hold where it is recaptured — writing a numbered PNG per frame.
/// Each frame advances the sim by exactly 1/30 s, so the sequence is the
/// model's real time however long a snapshot takes.
@MainActor
enum PlasmoidDemo {
    static func runIfRequested(_ m: EditorModel) async {
        let env = ProcessInfo.processInfo.environment
        if env["VCAD_PLASMOID"] == "1", env["VCAD_PLASMOID_SNAPSHOT_DIR"] == nil {
            for _ in 0..<240 where m.docPartMeshes.isEmpty { await wait(0.25) }
            PlasmoidStore.shared.attach(m); PlasmoidStore.shared.start()
            return
        }
        guard let dir = env["VCAD_PLASMOID_SNAPSHOT_DIR"] else { return }
        let url = URL(fileURLWithPath: dir)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        for _ in 0..<240 where m.docPartMeshes.isEmpty { await wait(0.25) }
        await wait(2)
        let store = PlasmoidStore.shared
        store.attach(m)
        let fdt = 1.0 / 30.0
        store.fixedFrameDt = fdt
        store.start()
        guard let b = store.binding else { log("no dish/magnet parts"); return }
        let ctl = ReleaseWindowController.shared
        var frame = 0
        func snap() async {
            RenderGate.wake()
            await wait(0.35)
            guard let ar = ctl.arView else { return }
            let name = String(format: "frame-%04d", frame)
            await withCheckedContinuation { (k: CheckedContinuation<Void, Never>) in
                ar.snapshot(saveToHDR: false) { img in
                    if let img, let tiff = img.tiffRepresentation, let rep = NSBitmapImageRep(data: tiff),
                       let png = rep.representation(using: .png, properties: [:]) {
                        try? png.write(to: url.appendingPathComponent(name + ".png"))
                    }
                    k.resume()
                }
            }
            let r = store.readout
            log(String(format: "%@ t=%.3f locked=%d beta=%.2f foot=(%.1f,%.1f)", name, r.t, r.locked ? 1 : 0,
                       r.beta, r.foot.x, r.foot.y))
            frame += 1
        }
        func advance(_ n: Int, every: Int = 1) async {
            for i in 0..<n {
                store.frame()
                ctl.refreshGeometry(model: m, collisions: false)
                if i % every == 0 { await snap() }
            }
        }
        // Hold: the racer comes round and locks under the magnet.
        m.selectParts([b.magnetPart]); await wait(0.5)
        await advance(45, every: 3)
        // Drag along the orbit through the real gizmo plane handle. The
        // magnet starts at 120° on the orbit; rays point straight down onto
        // the gizmo's XY plane at the cursor's (x, y).
        guard let c0 = m.gizmoCenterKernel() else { log("no gizmo centre"); return }
        let dish = PlasmoidBox(m.docPartMeshes[b.dishPart])
        let o = PlasmoidBinding.dishOrigin(dish)
        let r0 = simd_length(SIMD2<Double>(Double(c0.x) - o.x, Double(c0.y) - o.y))
        var ang = atan2(Double(c0.y) - o.y, Double(c0.x) - o.x)
        func ray(_ a: Double) -> (o: SIMD3<Float>, d: SIMD3<Float>) {
            let x = Float(o.x + r0 * cos(a)), y = Float(o.y + r0 * sin(a))
            return (SIMD3(x, y, c0.z + 300), SIMD3(0, 0, -1))
        }
        m.beginGizmoDrag(handle: "planeXY", ray: ray(ang))
        // Cost of one drag event (the document's in-place re-eval + scene
        // refresh) and of one sim frame, logged for the PR's numbers.
        var dragMs: [Double] = [], simMs: [Double] = []
        func dragStep(_ a: Double) {
            let t0 = CFAbsoluteTimeGetCurrent()
            m.gizmoDragTo(ray: ray(a))
            ctl.refreshGeometry(model: m, collisions: false)
            let t1 = CFAbsoluteTimeGetCurrent()
            store.frame()
            dragMs.append((t1 - t0) * 1000); simMs.append((CFAbsoluteTimeGetCurrent() - t1) * 1000)
        }
        // Slow drag against the race: 1.5 rad/s for 2 s — it follows.
        for i in 0..<60 {
            ang -= 1.5 * fdt
            dragStep(ang)
            if i % 2 == 0 { await snap() }
        }
        // Flick: 25 rad/s for 0.2 s — it breaks away.
        for _ in 0..<6 {
            ang -= 25 * fdt
            dragStep(ang)
            await snap()
        }
        let med = { (v: [Double]) -> Double in v.sorted()[v.count / 2] }
        log(String(format: "drag event (re-eval + refresh) median %.1f ms max %.1f ms; sim frame median %.2f ms",
                   med(dragMs), dragMs.max() ?? 0, med(simMs)))
        m.endGizmoDrag()
        ctl.refreshGeometry(model: m, collisions: true)
        // Hold still: recaptured on the next pass.
        await advance(45, every: 1)
        // 1 atm: the pin vanishes and the foot races off.
        store.setPressure(760)
        await advance(30, every: 2)
        log("done → \(dir)")
    }

    private static func wait(_ s: Double) async { try? await Task.sleep(nanoseconds: UInt64(s * 1e9)) }
    private static func log(_ s: String) { FileHandle.standardError.write(Data("[PLASMOID] \(s)\n".utf8)) }
}
