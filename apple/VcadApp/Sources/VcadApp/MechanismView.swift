#if os(macOS) || os(iOS) || os(visionOS)
#if canImport(AppKit)
import AppKit
#else
import UIKit
#endif
import RealityKit
import simd

// How a mechanism explains itself, without a diagram:
//
// - Touch: grabbing a jointed part sends a soft glow rippling out along the
//   joint graph (grabbed part, then its neighbours, then theirs), so you see
//   what is connected to what by feeling around. It fades on release.
// - Hold space: the solids go glassy and the kinematic skeleton shows — a dot
//   at every pivot (the driven joint in accent) and a line along every link —
//   and it keeps moving while you crank. Let go and the solids come back.
// - First open: a closed linkage walks one slow turn on its own, settles back,
//   and a single hint says "Drag anything".

/// Per-session presentation state (not observed: it drives RealityKit
/// entities directly, the same way the pose channel does).
@MainActor
final class MechanismPresentation {
    static let shared = MechanismPresentation()
    var skeletonShown = false
    var rippleToken = 0
    var walkedDocuments = Set<String>()
    var spaceMonitor: Any?
    /// Skeleton entities, rebuilt when the topology changes.
    var skeletonRoot: Entity?
    var skeletonKey = ""
    var segmentEntities: [ModelEntity] = []
    var pivotEntities: [ModelEntity] = []
    var governorTimer: Timer?
    var receiptToken = 0
    var receiptRunning = false
}

/// The pivots and link lines of the current pose, in kernel (centering) space.
struct MechanismSkeleton {
    struct Pivot { let point: SIMD3<Float>; let driven: Bool }
    var pivots: [Pivot] = []
    var segments: [(Int, Int)] = []
    /// Topology fingerprint (counts), to know when entities must be rebuilt.
    var key: String { "\(pivots.count)/\(segments.count)" }
}

extension EditorModel {
    // MARK: joint graph

    /// Instance-id adjacency through joints, skipping the ground (everything
    /// hangs off it, so going through it would light the whole machine).
    func jointNeighbours() -> [String: Set<String>] {
        let ground = documentJSON.flatMap(AsmEdit.groundInstanceId)
        var adj: [String: Set<String>] = [:]
        for j in asmJoints {
            guard let c = j["childInstanceId"] as? String, let p = j["parentInstanceId"] as? String,
                  c != ground, p != ground else { continue }
            adj[c, default: []].insert(p)
            adj[p, default: []].insert(c)
        }
        // Gear meshes connect bodies through a coupling, not a shared joint.
        let childOf = Dictionary(asmJoints.compactMap { j -> (String, String)? in
            guard let id = j["id"] as? String, let c = j["childInstanceId"] as? String else { return nil }
            return (id, c)
        }, uniquingKeysWith: { a, _ in a })
        for j in asmJoints {
            guard let c = j["childInstanceId"] as? String, let cpl = j["coupling"] as? [String: Any],
                  let d = cpl["driver"] as? String, let dc = childOf[d] else { continue }
            adj[c, default: []].insert(dc)
            adj[dc, default: []].insert(c)
        }
        // Welded parts (gears on an arbor, pins in a link) ride with their body.
        for j in asmJoints where (j["kind"] as? [String: Any])?["type"] as? String == "Fixed" {
            guard let c = j["childInstanceId"] as? String, let p = j["parentInstanceId"] as? String, p != ground else { continue }
            adj[c, default: []].insert(p); adj[p, default: []].insert(c)
        }
        return adj
    }

    /// Render indices in breadth-first layers out from instance `index`.
    func connectionLayers(from index: Int, maxLayers: Int = 6) -> [[Int]] {
        guard let start = instanceID(at: index) else { return [] }
        let adj = jointNeighbours()
        var seen: Set<String> = [start], frontier = [start], layers: [[Int]] = []
        for _ in 0..<maxLayers where !frontier.isEmpty {
            layers.append(frontier.compactMap(renderIndex(ofInstance:)))
            var next: [String] = []
            for id in frontier {
                for n in adj[id] ?? [] where seen.insert(n).inserted { next.append(n) }
            }
            frontier = next
        }
        return layers
    }

    // MARK: ripple

    /// Light the chain connected to `index`, one hop every 70 ms.
    func rippleConnections(from index: Int) {
        let pres = MechanismPresentation.shared
        pres.rippleToken &+= 1
        let token = pres.rippleToken
        let layers = connectionLayers(from: index)
        guard layers.count > 1 else { return }
        Task { @MainActor in
            for (k, layer) in layers.enumerated() {
                guard pres.rippleToken == token else { return }
                let strength = Float(max(0.25, 1.0 - Double(k) * 0.16))
                for i in layer { Self.glowInstance(i, strength: strength) }
                try? await Task.sleep(nanoseconds: 70_000_000)
            }
        }
    }

    /// Fade the ripple: put every body back to its normal paint.
    func endRipple() {
        let pres = MechanismPresentation.shared
        pres.rippleToken &+= 1
        let token = pres.rippleToken
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 220_000_000)
            guard pres.rippleToken == token else { return }
            ReleaseWindowController.shared.repaintAll()
        }
    }

    private static func glowInstance(_ i: Int, strength: Float) {
        guard let e = ReleaseWindowController.shared.centeringEntity?.findEntity(named: "inst\(i)") as? ModelEntity,
              var m = e.model?.materials.first as? PhysicallyBasedMaterial else { return }
        m.emissiveColor = .init(color: NSColor(srgbRed: 0.55, green: 0.75, blue: 1.0, alpha: 1))
        m.emissiveIntensity = 0.55 * strength
        e.model?.materials = [m]
        RenderGate.wake()
    }

    // MARK: skeleton

    /// Pivot points (world anchor of every joint) and link segments (the
    /// pivots each body carries, joined in order) for the current pose.
    func mechanismSkeleton() -> MechanismSkeleton {
        var sk = MechanismSkeleton()
        let ground = documentJSON.flatMap(AsmEdit.groundInstanceId)
        var pivotsOn: [String: [Int]] = [:]
        for j in asmJoints {
            guard let id = j["id"] as? String, let spec = jointSpec(id: id), spec.kind != .fixed else { continue }
            let pw = worldTransform(ofInstance: spec.parentInstanceId)
            let a = pw * SIMD4<Float>(Float(spec.parentAnchor.x), Float(spec.parentAnchor.y), Float(spec.parentAnchor.z), 1)
            let point = SIMD3(a.x, a.y, a.z)
            // A closure pin coincides with a tree pivot: reuse it.
            let k: Int
            if let existing = sk.pivots.firstIndex(where: { simd_distance($0.point, point) < 0.5 }) {
                k = existing
            } else {
                let driven = !isPassiveJoint(id) && jointCoupling(id) == nil
                sk.pivots.append(.init(point: point, driven: driven))
                k = sk.pivots.count - 1
            }
            for inst in [spec.parentInstanceId, spec.childInstanceId].compactMap({ $0 }) where inst != ground {
                if !(pivotsOn[inst]?.contains(k) ?? false) { pivotsOn[inst, default: []].append(k) }
            }
        }
        for (_, ps) in pivotsOn.sorted(by: { $0.key < $1.key }) where ps.count >= 2 {
            for a in 0..<ps.count { for b in (a + 1)..<ps.count { sk.segments.append((ps[a], ps[b])) } }
        }
        return sk
    }

    func setSkeletonShown(_ on: Bool) {
        let pres = MechanismPresentation.shared
        guard pres.skeletonShown != on, assemblyInstanceCount > 0 else { return }
        pres.skeletonShown = on
        guard let centering = ReleaseWindowController.shared.centeringEntity else { return }
        for i in 0..<assemblyInstanceCount {
            guard let e = centering.findEntity(named: "inst\(i)") else { continue }
            if on { e.components.set(OpacityComponent(opacity: 0.12)) } else { e.components.remove(OpacityComponent.self) }
        }
        if on { updateSkeleton() } else {
            pres.skeletonRoot?.removeFromParent()
            pres.skeletonRoot = nil
            pres.skeletonKey = ""
        }
        RenderGate.wake()
    }

    /// Re-pose the skeleton (called on every published pose while shown).
    func updateSkeleton() {
        let pres = MechanismPresentation.shared
        guard pres.skeletonShown, let centering = ReleaseWindowController.shared.centeringEntity else { return }
        let sk = mechanismSkeleton()
        let thick = max(displayedSceneSize * 0.006, 0.6)
        if pres.skeletonKey != sk.key || pres.skeletonRoot?.parent == nil {
            pres.skeletonRoot?.removeFromParent()
            let root = Entity()
            root.name = "mechanismSkeleton"
            let line = SimpleMaterial(color: NSColor(white: 0.92, alpha: 1), isMetallic: false)
            let dot = SimpleMaterial(color: NSColor(white: 1, alpha: 1), isMetallic: false)
            let accent = SimpleMaterial(color: NSColor(srgbRed: 0.94, green: 0.62, blue: 0.15, alpha: 1), isMetallic: false)
            let box = MeshResource.generateBox(size: 1)
            pres.segmentEntities = sk.segments.map { _ in
                let e = ModelEntity(mesh: box, materials: [line]); root.addChild(e); return e
            }
            pres.pivotEntities = sk.pivots.map { p in
                let e = ModelEntity(mesh: .generateSphere(radius: thick * (p.driven ? 2.6 : 1.8)), materials: [p.driven ? accent : dot])
                root.addChild(e); return e
            }
            centering.addChild(root)
            pres.skeletonRoot = root
            pres.skeletonKey = sk.key
        }
        for (k, p) in sk.pivots.enumerated() where k < pres.pivotEntities.count {
            pres.pivotEntities[k].position = p.point
        }
        for (k, s) in sk.segments.enumerated() where k < pres.segmentEntities.count {
            let a = sk.pivots[s.0].point, b = sk.pivots[s.1].point
            let d = b - a, len = simd_length(d)
            let e = pres.segmentEntities[k]
            e.position = (a + b) / 2
            e.scale = SIMD3(len, thick, thick)
            e.orientation = len > 1e-4 ? simd_quatf(from: SIMD3(1, 0, 0), to: d / len) : simd_quatf(ix: 0, iy: 0, iz: 0, r: 1)
        }
        RenderGate.wake()
    }

    /// Space bar held → skeleton (never while typing in a field).
    func installMechanismKeys() {
        let pres = MechanismPresentation.shared
        guard pres.spaceMonitor == nil else { return }
        #if os(macOS)   // no space bar on a phone: the skeleton is a Mac gesture
        pres.spaceMonitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp]) { [weak self] event in
            guard event.keyCode == 49, event.modifierFlags.intersection([.command, .option, .control]).isEmpty else { return event }
            if NSApp.keyWindow?.firstResponder is NSText { return event }
            let down = event.type == .keyDown, rep = event.isARepeat
            let handled = MainActor.assumeIsolated { () -> Bool in
                guard let self, self.assemblyInstanceCount > 0, !self.asmJoints.isEmpty else { return false }
                if down { if !rep { self.setSkeletonShown(true) } } else { self.setSkeletonShown(false) }
                return true
            }
            return handled ? nil : event
        }
        #endif
    }

    // MARK: first open

    /// A closed linkage (passive joints) walks one slow turn on first open.
    func walkOnFirstOpen() {
        let env = ProcessInfo.processInfo.environment
        guard env["VCAD_JOINTDRAG_SMOKE"] == nil, env["VCAD_NO_INTRO"] == nil,
              timeline == nil, asmJoints.contains(where: { ($0["passive"] as? Bool) == true || $0["coupling"] != nil }),
              let crank = asmJoints.first(where: { j in
                  guard let id = j["id"] as? String, let s = jointSpec(id: id) else { return false }
                  return s.kind == .revolute && !isPassiveJoint(id) && jointCoupling(id) == nil
              })?["id"] as? String else { return }
        let pres = MechanismPresentation.shared
        let crankName = (asmJoints.first { ($0["id"] as? String) == crank }?["name"] as? String) ?? ""
        let docKey = (documentDirectory ?? "") + "|" + crankName + "|\(assemblyInstanceCount)"
        guard pres.walkedDocuments.insert(docKey).inserted else { return }
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 700_000_000)
            guard let self, self.jointDrag == nil else { return }
            let v0 = self.currentJointValues()[crank] ?? 0
            let frames = 180
            for f in 1...frames {
                guard self.jointDrag == nil else { return }     // the user grabbed it
                let u = Double(f) / Double(frames)
                let ease = u * u * (3 - 2 * u)
                self.dragJoint(crank, to: v0 + 360 * ease)
                try? await Task.sleep(nanoseconds: 16_000_000)
            }
            self.assembly.dragValues.removeValue(forKey: crank)
            self.applyAssemblyPose(self.currentJointValues())
            GlassUI.shared.flash("Drag anything · hold space to see the linkage", symbol: "hand.draw")
        }
    }
}
#endif

#if os(macOS) || os(iOS) || os(visionOS)
// MARK: - Watt governor behavior
//
// A centrifugal governor's arms have no kinematic reason to rise: speed
// throws them out. For a document with a revolute joint named "Spindle" and a
// slider named "Sleeve", the sleeve height follows spindle speed with the
// conical-pendulum balance cos φ = g / (ω² L), lagged like the real inertia.
// Grab the spindle and spin it (or flick it) and the balls fly out; let it
// slow and they drop. Grab a ball and you lift the sleeve by hand.
extension EditorModel {
    func startGovernorIfPresent() {
        let pres = MechanismPresentation.shared
        pres.governorTimer?.invalidate()
        pres.governorTimer = nil
        func jid(_ n: String) -> String? { asmJoints.first { ($0["name"] as? String) == n }?["id"] as? String }
        guard let spindle = jid("Spindle"), let sleeve = jid("Sleeve"),
              let limits = jointSpec(id: sleeve)?.limits else { return }
        let armL = 0.070                                    // metres (pivot → ball)
        let twoA = (limits.upperBound + 6)                  // 2A, from the generator's limit
        var s = currentJointValues()[sleeve] ?? limits.lowerBound
        var omega = 0.0
        pres.governorTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 60, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                let dt = 1.0 / 60
                let drag = self.jointDrag
                let target: Double = (drag?.driverJointId == spindle) ? abs(drag!.velocity) * .pi / 180 : 0
                omega += (target - omega) * min(1, dt * 4)
                if drag?.driverJointId == sleeve { s = self.currentJointValues()[sleeve] ?? s; return }
                let c = omega > 0.1 ? min(1, 9.81 / (omega * omega * armL)) : 1
                let want = min(max(twoA * (1 - c), limits.lowerBound), limits.upperBound)
                let next = s + (want - s) * min(1, dt * 3)
                guard abs(next - s) > 0.005 else { return }
                s = next
                self.assembly.dragValues[sleeve] = s
                self.applyAssemblyPose(self.currentJointValues())
            }
        }
    }
}
#endif
