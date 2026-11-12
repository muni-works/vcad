import AVFoundation
import CVcadFFI
import Observation
import RealityKit
import SwiftUI
import simd

// Strike-to-hear: the Mallet tool.
//
// Arm the Mallet (M), click a part: vcad solves the part's vibration modes
// (crates/vcad-sound via `vcad_sound_call`, the solid modal solver), plays the
// strike — every mode ringing as loud as the mode shape moves at the struck
// point along the mallet's direction — and the part flexes on screen with its
// still regions (where to mount it) shown. The "Rings at" card names the
// pitch as a note with a tuner needle, shows the overtones as a spectrum
// (click a peak to hear and see that mode alone), and carries the Pitch
// constraint: lock it to a note and a chosen parameter is solved so the part
// stays on pitch.

// MARK: Model

struct SoundMaterialDTO: Decodable, Equatable {
    let name: String
    let youngs_modulus_mpa: Double
    let density_kg_m3: Double
}

struct SoundModeDTO: Decodable, Equatable {
    let hz: Double
    let note: String
    let cents: Double
    let shape: [Float]
}

struct PartModesDTO: Decodable, Equatable {
    let part: Int
    let name: String
    let material: SoundMaterialDTO
    let material_known: Bool
    let modes: [SoundModeDTO]
    let positions: [Float]
    let indices: [UInt32]
    let vertices: Int
    let ms: Double
}

struct PitchLockDTO: Codable, Equatable {
    var part: String
    var mode: Int
    var target_hz: Double
    var note: String?
    var drives: String
    var tolerance_cents: Double
}

struct LockStatusDTO: Decodable, Equatable {
    let lock: PitchLockDTO
    let hz: Double?
    let cents: Double?
    let holds: Bool?
    let error: String?
}

enum SoundFFI {
    /// Part names in visible-part order (the engine keeps names on features,
    /// not root nodes); sent with every request.
    nonisolated(unsafe) static var partNames: [String] = []

    static func call(doc: Data, _ request: [String: Any]) throws -> Data {
        var req = request
        if !partNames.isEmpty { req["part_names"] = partNames }
        return try AnalysisFFI.call(vcad_sound_call, doc: doc, request: try AnalysisFFI.encode(req))
    }

    static func modes(doc: Data, part: Int, quality: String = "preview",
                      mesh: (positions: [Float], indices: [UInt32])? = nil) throws -> PartModesDTO {
        var req: [String: Any] = ["op": "modes", "part": part, "modes": 6, "quality": quality]
        // The app's own tessellation: no document re-evaluation per strike.
        if let mesh { req["mesh"] = ["positions": mesh.positions, "indices": mesh.indices] }
        return try JSONDecoder().decode(PartModesDTO.self, from: call(doc: doc, req))
    }

    static func check(doc: Data, part: String? = nil) throws -> [LockStatusDTO] {
        struct R: Decodable { let locks: [LockStatusDTO] }
        var req: [String: Any] = ["op": "check"]
        if let part { req["part"] = part }
        return try JSONDecoder().decode(R.self, from: call(doc: doc, req)).locks
    }

    /// Solve + store a lock; returns the patched document and the final error in cents.
    static func lock(doc: Data, _ lock: PitchLockDTO) throws -> (document: [String: Any], cents: Double, converged: Bool) {
        var req = (try JSONSerialization.jsonObject(with: JSONEncoder().encode(lock)) as? [String: Any]) ?? [:]
        req["op"] = "lock"
        return try parseLock(try call(doc: doc, req))
    }

    static func parseLock(_ out: Data) throws -> (document: [String: Any], cents: Double, converged: Bool) {
        guard let obj = try JSONSerialization.jsonObject(with: out) as? [String: Any],
              let d = obj["document"] as? [String: Any],
              let rep = obj["report"] as? [String: Any] else { throw AnalysisFFI.Failure(message: "lock returned no document") }
        return (d, rep["cents"] as? Double ?? .nan, rep["converged"] as? Bool ?? false)
    }

    static func unlock(doc: Data, part: String, mode: Int) throws -> [String: Any] {
        let out = try call(doc: doc, ["op": "unlock", "part": part, "mode": mode])
        guard let obj = try JSONSerialization.jsonObject(with: out) as? [String: Any],
              let d = obj["document"] as? [String: Any] else { throw AnalysisFFI.Failure(message: "unlock returned no document") }
        return d
    }
}

enum Pitch {
    static let names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]
    static func hz(_ note: String) -> Double? {
        let s = note.trimmingCharacters(in: .whitespaces)
        guard let letter = s.first.map(String.init)?.uppercased(), var semis = ["C": 0, "D": 2, "E": 4, "F": 5, "G": 7, "A": 9, "B": 11][letter] else { return nil }
        var rest = s.dropFirst()
        if rest.first == "#" { semis += 1; rest = rest.dropFirst() } else if rest.first == "b" { semis -= 1; rest = rest.dropFirst() }
        guard let oct = Int(rest) else { return nil }
        return 440 * pow(2, Double((oct + 1) * 12 + semis - 69) / 12)
    }
    static func cents(_ a: Double, _ b: Double) -> Double { 1200 * log2(a / b) }
    /// Plain words for an offset: no raw cents.
    static func words(_ c: Double, target: String) -> (String, Color) {
        let a = abs(c), dir = c > 0 ? "sharp" : "flat"
        if a < 3 { return ("In tune with \(target)", .green) }
        if a < 10 { return ("A hair \(dir) of \(target), barely audible", .green) }
        if a < 40 { return ("Noticeably \(dir) of \(target)", .orange) }
        return ("Far from \(target)", .red)
    }
    /// Notes C3…C8 for pickers.
    static let pickable: [String] = (3...8).flatMap { o in names.map { "\($0)\(o)" } }.filter { Pitch.hz($0)! <= 4186.1 }
    /// Half-sine mallet contact spectrum (matches vcad-kernel-acoustics::strike::mallet_spectrum).
    static func mallet(_ hz: Double, contactMs: Double) -> Double {
        let tc = contactMs / 1000, u = 2 * hz * tc, d = abs(1 - u * u)
        if d < 1e-9 { return .pi / 4 }
        return abs(cos(.pi * hz * tc)) / d
    }
}

/// Decaying-sine synthesis, played on one engine.
final class ToneSynth: @unchecked Sendable {
    private let engine = AVAudioEngine()
    private let player = AVAudioPlayerNode()
    private let format = AVAudioFormat(standardFormatWithSampleRate: 48_000, channels: 1)!
    private var started = false

    init() {
        engine.attach(player)
        engine.connect(player, to: engine.mainMixerNode, format: format)
    }

    /// Render `partials` (Hz, gain, decay s) to a buffer.
    static func render(_ partials: [(hz: Double, gain: Double, decay: Double)], seconds: Double, rate: Double = 48_000) -> [Float] {
        let n = Int(seconds * rate)
        var out = [Float](repeating: 0, count: n)
        for p in partials where p.hz < rate * 0.45 && p.gain > 1e-4 {
            let w = 2 * Double.pi * p.hz / rate, k = exp(-1 / (p.decay * rate))
            var env = p.gain, phase = 0.0
            let attack = Int(0.002 * rate)
            for i in 0..<n {
                let a = i < attack ? Double(i) / Double(attack) : 1
                out[i] += Float(env * a * sin(phase))
                phase += w
                env *= k
                if env < 1e-5 { break }
            }
        }
        let peak = out.reduce(0) { max($0, abs($1)) }
        if peak > 0 { let s = 0.6 / peak; for i in out.indices { out[i] *= s } }
        return out
    }

    func play(_ samples: [Float]) {
        guard let buf = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(samples.count)) else { return }
        buf.frameLength = buf.frameCapacity
        samples.withUnsafeBufferPointer { buf.floatChannelData![0].update(from: $0.baseAddress!, count: samples.count) }
        if !engine.isRunning { try? engine.start(); started = engine.isRunning }
        player.stop()
        // Pause the engine once the strike has rung out: an idle running
        // engine keeps the audio IO thread busy for nothing.
        let engine = self.engine
        player.scheduleBuffer(buf, at: nil, options: [], completionCallbackType: .dataPlayedBack) { _ in
            DispatchQueue.main.async { engine.pause() }
        }
        player.play()
    }
}

@MainActor @Observable
final class SoundStore {
    static let shared = SoundStore()

    /// The Mallet tool is armed.
    var malletArmed = false { didSet { if !malletArmed { stopAnimation() } } }
    private(set) var modes: PartModesDTO?
    private(set) var busy = false
    private(set) var error: String?
    /// Mode shown alone (spectrum click); nil = the struck mix.
    var focusedMode: Int?
    /// Lock status for the analyzed part's fundamental, from `check`.
    private(set) var lockStatus: LockStatusDTO?
    /// Target note in the Pitch row.
    var targetNote = "G6"
    /// Parameter the lock drives.
    var drives: String?

    @ObservationIgnored private var key: (Data, Int, Int)?
    @ObservationIgnored private let synth = ToneSynth()
    @ObservationIgnored weak var model: EditorModel?
    /// Animation state read by the overlay.
    @ObservationIgnored private(set) var animStart: Date?
    @ObservationIgnored private(set) var animWeights: [Double] = []
    @ObservationIgnored private var timer: Timer?
    /// Bumped to rebuild the overlay entity (new mesh).
    private(set) var overlayRevision = 0

    var shown: Bool { malletArmed }

    /// Document parameters a lock can drive.
    func parameterNames(_ m: EditorModel) -> [String] {
        ((m.documentJSON?["parameters"] as? [String: Any])?.keys).map { $0.sorted() } ?? []
    }

    /// Solve (or reuse) the modes of `part`.
    func ensureModes(part: Int, _ m: EditorModel) async -> PartModesDTO? {
        model = m
        SoundStore.syncNames(m)
        guard let data = AnalysisStore.documentData(m) else { return nil }
        if let k = key, k.0 == data, k.1 == part, k.2 == m.sceneGeneration, let modes { return modes }
        busy = true
        error = nil
        let mesh: (positions: [Float], indices: [UInt32])? = part < m.docPartMeshes.count
            ? (m.docPartMeshes[part].positions.flatMap { [$0.x, $0.y, $0.z] }, m.docPartMeshes[part].indices)
            : nil
        let result = await Task.detached(priority: .userInitiated) { () -> Result<PartModesDTO, Error> in
            Result { try SoundFFI.modes(doc: data, part: part, mesh: mesh) }
        }.value
        busy = false
        switch result {
        case .success(let pm):
            modes = pm
            key = (data, part, m.sceneGeneration)
            focusedMode = nil
            overlayRevision &+= 1
            drives = drivingParameters(m, part: pm.name).first
            if let f = pm.modes.first { targetNote = closestPickable(f.hz) }
            refreshLock(m)
            return pm
        case .failure(let e):
            error = "\(e)"
            return nil
        }
    }

    /// Re-analyze the part named `name` after an edit (edits can reorder
    /// parts, so the old index may now be a different part).
    func reanalyze(_ name: String, _ m: EditorModel, fallback: Int, after gen: Int? = nil) async {
        // An edit re-tessellates asynchronously: wait (≤ 5 s) for the new
        // scene so the solve doesn't run on the old mesh.
        if let gen {
            for _ in 0..<50 where m.sceneGeneration == gen {
                try? await Task.sleep(nanoseconds: 100_000_000)
            }
        }
        SoundStore.syncNames(m)
        let idx = SoundFFI.partNames.firstIndex(of: name) ?? fallback
        _ = await ensureModes(part: idx, m)
    }

    /// Visible-part names from the feature tree.
    static func syncNames(_ m: EditorModel) {
        let byPart = Dictionary(m.featureNodes.compactMap { n in n.partIndex.map { ($0, n.name) } }, uniquingKeysWith: { a, _ in a })
        let count = (byPart.keys.max() ?? -1) + 1
        SoundFFI.partNames = (0..<count).map { byPart[$0] ?? "part\($0)" }
    }

    /// Parameters that drive this part: names whose binding expressions
    /// mention them on any node — narrowed to ones containing the part's
    /// name suffix when that matches (glockenspiel `len_c6` for `bar_c6`).
    func drivingParameters(_ m: EditorModel, part: String) -> [String] {
        let all = parameterNames(m)
        let suffix = part.split(separator: "_").last.map(String.init) ?? part
        let mine = all.filter { $0.hasSuffix("_" + suffix) || $0 == suffix }
        return mine.isEmpty ? all : mine + all.filter { !mine.contains($0) }
    }

    private func closestPickable(_ hz: Double) -> String {
        Pitch.pickable.min { abs(Pitch.cents(hz, Pitch.hz($0)!)) < abs(Pitch.cents(hz, Pitch.hz($1)!)) } ?? "A4"
    }

    /// Strike at animation-mesh vertex `vertex` along `dir` (kernel frame).
    func strike(vertex: Int, dir: SIMD3<Float>) {
        guard let pm = modes else { return }
        let weights = pm.modes.map { m -> Double in
            guard m.shape.count >= 3 * (vertex + 1) else { return 0 }
            let u = SIMD3(m.shape[3 * vertex], m.shape[3 * vertex + 1], m.shape[3 * vertex + 2])
            return Double(abs(simd_dot(u, dir))) * Pitch.mallet(m.hz, contactMs: 1)
        }
        let top = weights.max() ?? 0
        let g = top > 0 ? weights.map { $0 / top } : weights
        focusedMode = nil
        play(g)
    }

    /// Play one mode alone (spectrum click).
    func playMode(_ i: Int) {
        guard let pm = modes, i < pm.modes.count else { return }
        focusedMode = i
        play(pm.modes.indices.map { $0 == i ? 1 : 0 })
    }

    private func play(_ g: [Double]) {
        guard let pm = modes else { return }
        let f1 = pm.modes.first?.hz ?? 440
        let partials = zip(pm.modes, g).map { (m, w) in (hz: m.hz, gain: w, decay: max(0.25, 2.2 * sqrt(f1 / m.hz))) }
        let samples = ToneSynth.render(partials, seconds: 2.6)
        synth.play(samples)
        animWeights = g
        animStart = Date()
        startAnimation()
    }

    // MARK: Animation

    /// Per-vertex displacement scale (mm) at time `t` since the strike: each
    /// mode swings at a slowed, watchable rate, decaying like the sound.
    func displacement(at t: Double) -> [Float]? {
        guard let pm = modes, t < 2.4 else { return nil }
        var out = [Float](repeating: 0, count: pm.positions.count)
        let size = boundingSize(pm)
        for (i, m) in pm.modes.enumerated() where i < animWeights.count && animWeights[i] > 0.02 {
            let visualHz = 2.2 + Double(i) * 1.3
            let a = Float(animWeights[i] * exp(-t * 1.6) * sin(2 * .pi * visualHz * t)) * size * 0.06
            m.shape.withUnsafeBufferPointer { s in
                for k in 0..<min(out.count, s.count) { out[k] += a * s[k] }
            }
        }
        return out
    }

    private func boundingSize(_ pm: PartModesDTO) -> Float {
        var lo = SIMD3<Float>(repeating: .infinity), hi = SIMD3<Float>(repeating: -.infinity)
        for i in stride(from: 0, to: pm.positions.count, by: 3) {
            let p = SIMD3(pm.positions[i], pm.positions[i + 1], pm.positions[i + 2])
            lo = simd_min(lo, p); hi = simd_max(hi, p)
        }
        return simd_reduce_max(hi - lo)
    }

    private func startAnimation() {
        timer?.invalidate()
        timer = Timer.scheduledTimer(withTimeInterval: 1.0 / 30, repeats: true) { _ in
            Task { @MainActor in SoundStore.shared.tick() }
        }
    }

    func stopAnimation() {
        timer?.invalidate()
        timer = nil
        animStart = nil
        tick()
    }

    /// Per-frame update hook (set by the viewport).
    @ObservationIgnored var onFrame: ((SoundStore) -> Void)?

    private func tick() {
        onFrame?(self)
        if let s = animStart, Date().timeIntervalSince(s) > 2.5 { timer?.invalidate(); timer = nil; animStart = nil; onFrame?(self) }
    }

    // MARK: Pitch lock

    func refreshLock(_ m: EditorModel) {
        guard let pm = modes, let data = AnalysisStore.documentData(m) else { lockStatus = nil; return }
        let name = pm.name
        guard ((m.documentJSON?["pitch_locks"] as? [[String: Any]]) ?? []).contains(where: { ($0["part"] as? String) == name }) else {
            lockStatus = nil; return
        }
        // The check is a Fine single-part solve (~2 s): reuse it per bytes+part.
        if let c = lockCache, c.data == data, c.part == name { apply(c.status); return }
        Task.detached(priority: .utility) {
            let st = (try? SoundFFI.check(doc: data, part: name))?.first { $0.lock.part == name && $0.lock.mode == 1 }
            await MainActor.run {
                SoundStore.shared.lockCache = (data, name, st)
                SoundStore.shared.apply(st)
            }
        }
    }

    private var lockCache: (data: Data, part: String, status: LockStatusDTO?)?

    private func apply(_ st: LockStatusDTO?) {
        lockStatus = st
        if let n = st?.lock.note { targetNote = n }
        drives = st?.lock.drives ?? drives
    }

    var isLocked: Bool { lockStatus != nil }

    /// Re-solve the stored lock against the current geometry.
    func resolveLock(_ m: EditorModel) async {
        guard let st = lockStatus, let data = AnalysisStore.documentData(m) else { return }
        busy = true
        defer { busy = false }
        do {
            let lock = st.lock
            let raw = try await Task.detached(priority: .userInitiated) { () throws -> Data in
                var req = (try JSONSerialization.jsonObject(with: JSONEncoder().encode(lock)) as? [String: Any]) ?? [:]
                req["op"] = "lock"
                return try SoundFFI.call(doc: data, req)
            }.value
            let r = try SoundFFI.parseLock(raw)
            let gen = m.sceneGeneration
            _ = try AgentEdits.edit(m, name: "Re-solve pitch \(lock.note ?? "")") { try $0.loadIR(r.document) }
            if let pm = modes { await reanalyze(pm.name, m, fallback: pm.part, after: gen) }
        } catch {
            self.error = "\(error)"
        }
    }

    func toggleLock(_ m: EditorModel) async {
        guard let pm = modes, let data = AnalysisStore.documentData(m) else { return }
        busy = true
        defer { busy = false }
        do {
            if isLocked {
                let d = try SoundFFI.unlock(doc: data, part: pm.name, mode: 1)
                _ = try AgentEdits.edit(m, name: "Unlock pitch") { try $0.loadIR(d) }
                lockStatus = nil
            } else {
                guard let drives, let hz = Pitch.hz(targetNote) else { error = "Pick a parameter to drive."; return }
                let lock = PitchLockDTO(part: pm.name, mode: 1, target_hz: hz, note: targetNote, drives: drives, tolerance_cents: 1)
                // Solve off the main actor; hand back bytes (Sendable), parse here.
                let raw = try await Task.detached(priority: .userInitiated) { () throws -> Data in
                    var req = (try JSONSerialization.jsonObject(with: JSONEncoder().encode(lock)) as? [String: Any]) ?? [:]
                    req["op"] = "lock"
                    return try SoundFFI.call(doc: data, req)
                }.value
                let r = try SoundFFI.parseLock(raw)
                let gen = m.sceneGeneration
                _ = try AgentEdits.edit(m, name: "Lock pitch to \(targetNote)") { try $0.loadIR(r.document) }
                if !r.converged { error = String(format: "Got within %.1f cents of %@ — %@ may be at its limit.", abs(r.cents), targetNote, drives) }
                await reanalyze(pm.name, m, fallback: pm.part, after: gen)
            }
        } catch {
            self.error = "\(error)"
        }
    }
}

// MARK: Viewport overlay

/// The struck part, flexing: a copy of its refined mesh deformed each frame,
/// plus its still regions in green. The real part hides while it rings.
@MainActor
func syncSoundOverlay(_ store: SoundStore, in parent: Entity) {
    let name = "soundRoot-\(store.overlayRevision)"
    guard store.malletArmed, let pm = store.modes, !pm.indices.isEmpty else {
        parent.children.filter { $0.name.hasPrefix("soundRoot") }.forEach { $0.removeFromParent() }
        store.onFrame = nil
        return
    }
    if parent.findEntity(named: name) != nil { return }
    parent.children.filter { $0.name.hasPrefix("soundRoot") }.forEach { $0.removeFromParent() }
    let root = Entity()
    root.name = name
    let base = (0..<pm.vertices).map { SIMD3(pm.positions[3 * $0], pm.positions[3 * $0 + 1], pm.positions[3 * $0 + 2]) }
    // Smooth vertex normals once (the deflection is small; lighting needn't follow it).
    var normals = [SIMD3<Float>](repeating: .zero, count: base.count)
    for t in stride(from: 0, to: pm.indices.count, by: 3) {
        let (a, b, c) = (Int(pm.indices[t]), Int(pm.indices[t + 1]), Int(pm.indices[t + 2]))
        let n = simd_cross(base[b] - base[a], base[c] - base[a])
        normals[a] += n; normals[b] += n; normals[c] += n
    }
    normals = normals.map { simd_length($0) > 0 ? simd_normalize($0) : SIMD3(0, 0, 1) }
    // LowLevelMesh: one vertex buffer, positions rewritten in place each frame —
    // regenerating a MeshResource per frame pegged the CPU.
    struct V { var p: SIMD3<Float>; var n: SIMD3<Float> }
    var desc = LowLevelMesh.Descriptor()
    desc.vertexAttributes = [
        .init(semantic: .position, format: .float3, offset: 0),
        .init(semantic: .normal, format: .float3, offset: MemoryLayout<SIMD3<Float>>.stride),
    ]
    desc.vertexLayouts = [.init(bufferIndex: 0, bufferStride: MemoryLayout<V>.stride)]
    desc.vertexCapacity = base.count
    desc.indexCapacity = pm.indices.count
    desc.indexType = .uint32
    guard let llm = try? LowLevelMesh(descriptor: desc) else { return }
    var lo = SIMD3<Float>(repeating: .infinity), hi = SIMD3<Float>(repeating: -.infinity)
    for p in base { lo = simd_min(lo, p); hi = simd_max(hi, p) }
    let pad = (hi - lo) * 0.25
    llm.withUnsafeMutableIndices { raw in
        let dst = raw.bindMemory(to: UInt32.self)
        for (i, v) in pm.indices.enumerated() { dst[i] = v }
    }
    func write(_ d: [Float]?) {
        llm.withUnsafeMutableBytes(bufferIndex: 0) { raw in
            let dst = raw.bindMemory(to: V.self)
            for i in 0..<base.count {
                let off = d.map { SIMD3($0[3 * i], $0[3 * i + 1], $0[3 * i + 2]) } ?? .zero
                dst[i] = V(p: base[i] + off, n: normals[i])
            }
        }
    }
    write(nil)
    llm.parts.replaceAll([LowLevelMesh.Part(indexCount: pm.indices.count, topology: .triangle,
                                            bounds: BoundingBox(min: lo - pad, max: hi + pad))])
    var mat = PhysicallyBasedMaterial()
    mat.baseColor = .init(tint: NSColor.systemTeal)
    mat.metallic = 0.6
    mat.roughness = 0.35
    mat.faceCulling = .none
    guard let mesh = try? MeshResource(from: llm) else { return }
    let body = ModelEntity(mesh: mesh, materials: [mat])
    body.name = "soundBody"
    body.isEnabled = false
    root.addChild(body)
    // Still regions of the mode in view: triangles whose vertices all move < 10% of the peak.
    func stillMesh(_ mode: Int) -> MeshResource? {
        guard mode < pm.modes.count else { return nil }
        let s = pm.modes[mode].shape
        func amp(_ v: UInt32) -> Float { let i = Int(v); return simd_length(SIMD3(s[3 * i], s[3 * i + 1], s[3 * i + 2])) }
        var pos: [SIMD3<Float>] = [], idx: [UInt32] = []
        for t in stride(from: 0, to: pm.indices.count, by: 3) {
            let (a, b, c) = (pm.indices[t], pm.indices[t + 1], pm.indices[t + 2])
            guard amp(a) < 0.1, amp(b) < 0.1, amp(c) < 0.1 else { continue }
            let n = simd_normalize(simd_cross(base[Int(b)] - base[Int(a)], base[Int(c)] - base[Int(a)]))
            let off = n.x.isFinite ? n * 0.05 : .zero
            let o = UInt32(pos.count)
            pos += [base[Int(a)] + off, base[Int(b)] + off, base[Int(c)] + off]
            idx += [o, o + 1, o + 2]
        }
        guard !idx.isEmpty else { return nil }
        var d = MeshDescriptor(name: "still")
        d.positions = MeshBuffers.Positions(pos)
        d.primitives = .triangles(idx)
        return try? MeshResource.generate(from: [d])
    }
    var still = UnlitMaterial(color: NSColor.systemGreen.withAlphaComponent(0.85))
    still.faceCulling = .none
    let stillEntity = ModelEntity()
    stillEntity.name = "soundStill"
    if let m = stillMesh(0) { stillEntity.model = ModelComponent(mesh: m, materials: [still]) }
    root.addChild(stillEntity)
    parent.addChild(root)

    var stillMode = 0
    store.onFrame = { [weak body, weak stillEntity, weak parent] s in
        guard let body, let parent else { return }
        let real = parent.findEntity(named: "part\(pm.part)")
        let want = s.focusedMode ?? 0
        if want != stillMode, let stillEntity {
            stillMode = want
            stillEntity.model = stillMesh(want).map { ModelComponent(mesh: $0, materials: [still]) }
        }
        guard let start = s.animStart, let d = s.displacement(at: Date().timeIntervalSince(start)) else {
            body.isEnabled = false
            real?.isEnabled = true
            return
        }
        write(d)
        body.isEnabled = true
        real?.isEnabled = false
    }
}

/// Ray–triangle hit on the analyzed part's animation mesh (kernel mm).
/// Returns the nearest vertex of the hit triangle and the inward strike direction.
func soundHit(_ pm: PartModesDTO, origin o: SIMD3<Float>, dir d: SIMD3<Float>) -> (vertex: Int, dir: SIMD3<Float>)? {
    var best: (t: Float, tri: Int)?
    for t in stride(from: 0, to: pm.indices.count, by: 3) {
        func p(_ k: Int) -> SIMD3<Float> { let i = Int(pm.indices[t + k]); return SIMD3(pm.positions[3 * i], pm.positions[3 * i + 1], pm.positions[3 * i + 2]) }
        let (a, b, c) = (p(0), p(1), p(2))
        let e1 = b - a, e2 = c - a, h = simd_cross(d, e2), det = simd_dot(e1, h)
        if abs(det) < 1e-9 { continue }
        let f = 1 / det, s = o - a, u = f * simd_dot(s, h)
        if u < 0 || u > 1 { continue }
        let q = simd_cross(s, e1), v = f * simd_dot(d, q)
        if v < 0 || u + v > 1 { continue }
        let tt = f * simd_dot(e2, q)
        if tt > 0, tt < (best?.t ?? .infinity) { best = (tt, t) }
    }
    guard let b = best else { return nil }
    let hit = o + d * b.t
    let tri = (0..<3).map { Int(pm.indices[b.tri + $0]) }
    let v = tri.min { a, c in
        simd_distance(SIMD3(pm.positions[3 * a], pm.positions[3 * a + 1], pm.positions[3 * a + 2]), hit)
            < simd_distance(SIMD3(pm.positions[3 * c], pm.positions[3 * c + 1], pm.positions[3 * c + 2]), hit)
    }!
    return (v, simd_normalize(d))
}

// MARK: Card

struct RingsAtCard: View {
    @Bindable var model: EditorModel
    @Bindable var store = SoundStore.shared

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            PanelHeader(title: "Mallet", systemImage: "hammer") { store.malletArmed = false }
            if let pm = store.modes, let f = pm.modes.first {
                ringsAt(pm, f)
                spectrum(pm)
                pitchRow(pm)
                if !pm.material_known {
                    Text("Material “\(pm.material.name)” assumed — set the part's material for the real pitch.")
                        .font(.caption).foregroundStyle(.orange).fixedSize(horizontal: false, vertical: true)
                }
                Divider()
                Button { TuneStore.shared.shown = true } label: {
                    Label("Check the real part", systemImage: "mic")
                }
                .buttonStyle(.plain).font(.caption).foregroundStyle(.secondary)
            } else if store.busy {
                HStack { ProgressView().controlSize(.small); Text("Listening to the geometry…").foregroundStyle(.secondary) }
            } else {
                Text("Click a part to strike it.").foregroundStyle(.secondary)
            }
            if let e = store.error {
                Text(e).font(.caption).foregroundStyle(.red).fixedSize(horizontal: false, vertical: true)
            }
        }
        .controlSize(.small)
        .padding(Theme.Space.m).frame(width: Theme.Width.toolInspector).panelSurface()
        .accessibilityIdentifier("mallet-card")
        .task(id: model.documentRevision) {
            if let pm = store.modes { await store.reanalyze(pm.name, model, fallback: pm.part) }
        }
    }

    private var targetHz: Double { Pitch.hz(store.targetNote) ?? 440 }

    private func ringsAt(_ pm: PartModesDTO, _ f: SoundModeDTO) -> some View {
        let c = Pitch.cents(f.hz, targetHz)
        let (words, color) = Pitch.words(c, target: store.targetNote)
        return VStack(alignment: .leading, spacing: 4) {
            Text("\(pm.name) rings at").font(.caption).foregroundStyle(.secondary)
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(f.note).font(.system(size: 30, weight: .semibold))
                Text(String(format: "%.0f Hz", f.hz)).foregroundStyle(.secondary).monospacedDigit()
                if store.busy { ProgressView().controlSize(.mini) }
            }
            TunerNeedle(cents: c).frame(height: 34)
            Text(words).font(.callout).foregroundStyle(color)
        }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("mallet-rings-at")
    }

    private func spectrum(_ pm: PartModesDTO) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Overtones — click to hear one alone").font(.caption).foregroundStyle(.secondary)
            HStack(alignment: .bottom, spacing: 6) {
                ForEach(Array(pm.modes.enumerated()), id: \.offset) { i, m in
                    Button { store.playMode(i) } label: {
                        VStack(spacing: 2) {
                            Text(m.note).font(.caption2).monospaced()
                            RoundedRectangle(cornerRadius: 2)
                                .fill(store.focusedMode == i ? Color.accentColor : .secondary.opacity(0.6))
                                .frame(width: 6, height: max(8, 40 * CGFloat(pow(pm.modes[0].hz / m.hz, 0.5))))
                            Text(i == 0 ? "1×" : String(format: "%.1f×", m.hz / pm.modes[0].hz)).font(.caption2).foregroundStyle(.secondary)
                        }
                        .frame(minWidth: 26)
                    }
                    .buttonStyle(.plain)
                    .help(String(format: "Mode %d · %.0f Hz", i + 1, m.hz))
                    .accessibilityIdentifier("mallet-mode-\(i + 1)")
                }
            }
        }
    }

    private func pitchRow(_ pm: PartModesDTO) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Constraint").font(.caption).foregroundStyle(.secondary)
            HStack(spacing: 6) {
                Image(systemName: "music.note").foregroundStyle(.secondary)
                Text("Pitch")
                Spacer()
                Picker("", selection: $store.targetNote) {
                    ForEach(Pitch.pickable, id: \.self) { Text($0).tag($0) }
                }
                .labelsHidden().frame(width: 70)
                .disabled(store.isLocked)
                Button { Task { await store.toggleLock(model) } } label: {
                    Image(systemName: store.isLocked ? "lock.fill" : "lock.open")
                }
                .help(store.isLocked ? "Unlock pitch" : "Lock pitch: solve the driven dimension to stay on this note")
                .accessibilityIdentifier("mallet-lock")
            }
            .padding(6)
            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(store.isLocked ? Color.accentColor : Color.secondary.opacity(0.3)))
            let params = store.drivingParameters(model, part: pm.name)
            if params.isEmpty {
                Text("Add a parameter (e.g. a length) for the pitch lock to drive.").font(.caption).foregroundStyle(.secondary)
            } else {
                HStack {
                    Text("drives").font(.caption).foregroundStyle(.secondary)
                    Picker("", selection: Binding(get: { store.drives ?? params[0] }, set: { store.drives = $0 })) {
                        ForEach(params, id: \.self) { Text($0).tag($0) }
                    }
                    .labelsHidden().disabled(store.isLocked)
                }
            }
            if let st = store.lockStatus, let c = st.cents {
                if st.holds == true {
                    Text("Locked · on pitch").font(.caption).foregroundStyle(.green)
                } else {
                    HStack {
                        Text(String(format: "Locked · %.0f cents %@", abs(c), c < 0 ? "flat" : "sharp"))
                            .font(.caption).foregroundStyle(.orange)
                        Spacer()
                        Button(store.busy ? "Solving…" : "Re-solve") { Task { await store.resolveLock(model) } }
                            .disabled(store.busy)
                            .accessibilityIdentifier("mallet-resolve")
                    }
                }
            }
        }
    }
}

struct TunerNeedle: View {
    let cents: Double
    var body: some View {
        Canvas { ctx, size in
            let cx = size.width / 2, cy = size.height, r = min(size.width / 2 - 6, size.height - 4)
            var arc = Path()
            arc.addArc(center: CGPoint(x: cx, y: cy), radius: r, startAngle: .degrees(200), endAngle: .degrees(340), clockwise: false)
            ctx.stroke(arc, with: .color(.secondary.opacity(0.4)), lineWidth: 1)
            var ok = Path()
            ok.addArc(center: CGPoint(x: cx, y: cy), radius: r, startAngle: .degrees(263), endAngle: .degrees(277), clockwise: false)
            ctx.stroke(ok, with: .color(.green), lineWidth: 4)
            let a = Angle.degrees(270 + max(-50, min(50, cents)) * 1.4).radians
            var n = Path()
            n.move(to: CGPoint(x: cx, y: cy))
            n.addLine(to: CGPoint(x: cx + cos(a) * (r - 2), y: cy + sin(a) * (r - 2)))
            ctx.stroke(n, with: .color(.primary), lineWidth: 2)
        }
        .accessibilityHidden(true)
    }
}


/// The part a kernel-frame ray actually hits first, by ray-casting each part's
/// render mesh (collision bounds of an enclosing part — a frame under its
/// bars — would otherwise win).
@MainActor
func nearestPartHit(_ m: EditorModel, origin o: SIMD3<Float>, dir d: SIMD3<Float>) -> Int? {
    var best: (t: Float, part: Int)?
    for (pi, pm) in m.docPartMeshes.enumerated() {
        // Slab test against the part's bounds first.
        var t0: Float = 0, t1: Float = .infinity
        var miss = false
        for a in 0..<3 {
            if abs(d[a]) < 1e-12 {
                if o[a] < pm.lo[a] || o[a] > pm.hi[a] { miss = true; break }
                continue
            }
            var ta = (pm.lo[a] - o[a]) / d[a], tb = (pm.hi[a] - o[a]) / d[a]
            if ta > tb { swap(&ta, &tb) }
            t0 = max(t0, ta); t1 = min(t1, tb)
            if t0 > t1 { miss = true; break }
        }
        if miss || t0 > (best?.t ?? .infinity) { continue }
        for t in stride(from: 0, to: pm.indices.count, by: 3) {
            let a = pm.positions[Int(pm.indices[t])], b = pm.positions[Int(pm.indices[t + 1])], c = pm.positions[Int(pm.indices[t + 2])]
            let e1 = b - a, e2 = c - a, h = simd_cross(d, e2), det = simd_dot(e1, h)
            if abs(det) < 1e-12 { continue }
            let f = 1 / det, s = o - a, u = f * simd_dot(s, h)
            if u < 0 || u > 1 { continue }
            let q = simd_cross(s, e1), v = f * simd_dot(d, q)
            if v < 0 || u + v > 1 { continue }
            let tt = f * simd_dot(e2, q)
            if tt > 0, tt < (best?.t ?? .infinity) { best = (tt, pi) }
        }
    }
    return best?.part
}
