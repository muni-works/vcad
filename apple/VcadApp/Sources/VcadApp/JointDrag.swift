import Foundation
import simd
#if canImport(AppKit)
import AppKit
#else
import UIKit
#endif

// Direct joint drag: grab any assembly instance moved by a revolute or slider
// joint and pull it. The mechanism moves, not the camera.
//
// The solve is closed-form: a revolute drag intersects the cursor ray with
// the joint's plane (through the pivot, normal to the axis) and measures the
// swept angle; a slider drag takes the closest point on the axis line. One FK
// solve per frame, through the same `dragJoint` channel the Joints panel
// sliders use, so limits, the live pose and the one-undo-step commit are
// shared.
//
// Gear couplings (`joint.coupling = {driver, ratio, offset}`) are resolved by
// the kernel FK. Grabbing a driven gear back-drives its root driver through
// the inverse ratio, so you can turn any gear in a train.
//
// Release with speed and the joint coasts (flick), decaying on bearing
// friction scaled by the moving parts' size, then commits once when it
// settles. Gears tick per tooth: a trackpad haptic detent plus a soft click.

/// What a live joint drag is driving and how the cursor maps onto it.
struct JointDragSession {
    /// The joint under the hand (its pivot/axis define the gesture).
    let grabbedJointId: String
    /// The root driver actually written (== grabbed unless coupled).
    let driverJointId: String
    /// d(grabbed)/d(driver) along the coupling chain (1 when uncoupled).
    let chainRatio: Double
    let isSlider: Bool
    /// Joint pivot and axis in kernel space at the grab (the parent is
    /// unaffected by its own child joint, so these hold for the whole drag).
    let pivot: SIMD3<Float>
    let axis: SIMD3<Float>
    /// Grabbed point, kernel space (label anchor).
    var anchor: SIMD3<Float>
    /// Previous in-plane vector (revolute) or axis parameter (slider).
    var lastVec: SIMD3<Float>
    var lastParam: Float
    /// Driver value now, and its velocity (units/s) for the flick.
    var value: Double
    var velocity: Double = 0
    var lastTime: TimeInterval
    /// Tooth pitch of the driver's gear (degrees per tooth), when it is one.
    let toothPitchDeg: Double?
    var lastTooth: Int
    /// Grabbed a passive link of a closed linkage: no pivot to rotate about,
    /// so each frame searches the mechanism's driver for the value that puts
    /// this point (grabbed instance's local frame) under the cursor.
    var numeric: (instance: Int, local: SIMD4<Float>)? = nil
}

extension EditorModel {
    // MARK: joint lookup

    /// Joint JSON whose child is `instanceId`.
    private func jointDict(forChild instanceId: String) -> [String: Any]? {
        asmJoints.first { ($0["childInstanceId"] as? String) == instanceId }
    }

    /// The joint that moves instance `index`: its own revolute/slider joint
    /// or, through Fixed welds, the first movable joint up the chain.
    func drivingJointId(forInstance index: Int) -> String? {
        guard let start = instanceID(at: index) else { return nil }
        var id: String? = start
        var seen = Set<String>()
        while let cur = id, seen.insert(cur).inserted {
            guard let j = jointDict(forChild: cur), let jid = j["id"] as? String,
                  let spec = jointSpec(id: jid) else { return nil }
            switch spec.kind {
            case .revolute, .slider: return jid
            case .fixed: id = spec.parentInstanceId
            default: return nil
            }
        }
        return nil
    }

    /// Instance id at FFI/render index `i` (the index space of
    /// `instanceTransforms` and the `inst<i>` entities).
    func instanceID(at i: Int) -> String? {
        i >= 0 && i < assemblyInstanceIDs.count ? assemblyInstanceIDs[i] : nil
    }

    /// FFI/render index of instance `id`.
    func renderIndex(ofInstance id: String) -> Int? { assemblyInstanceIDs.firstIndex(of: id) }

    /// Tooth count of the SpurGear an instance's part is built from, if any.
    func gearTeeth(instance index: Int) -> Int? { gearSpec(instance: index)?.teeth }

    /// Teeth and module of the SpurGear an instance's part is built from.
    func gearSpec(instance index: Int) -> (teeth: Int, module: Double)? {
        guard let json = documentJSON, let iid = instanceID(at: index),
              let defId = asmInstances.first(where: { ($0["id"] as? String) == iid })?["partDefId"] as? String,
              let root = AsmEdit.partDefs(json).first(where: { $0.id == defId })?.root,
              let nodes = json["nodes"] as? [String: Any] else { return nil }
        var stack = [root], seen = Set<Int>()
        while let n = stack.popLast(), seen.insert(n).inserted {
            guard let node = nodes[String(n)] as? [String: Any],
                  let op = node["op"] as? [String: Any] else { continue }
            if op["type"] as? String == "SpurGear",
               let z = (op["teeth"] as? NSNumber)?.intValue,
               let m = (op["module"] as? NSNumber)?.doubleValue { return (z, m) }
            // Follow operand references (transform chains, booleans).
            for key in ["child", "left", "right", "base"] {
                if let c = (op[key] as? NSNumber)?.intValue { stack.append(c) }
            }
            if let cs = op["children"] as? [NSNumber] { stack.append(contentsOf: cs.map(\.intValue)) }
        }
        return nil
    }

    /// Kernel-space world transform of an instance (identity for nil/world).
    func worldTransform(ofInstance id: String?) -> float4x4 {
        guard let id, let i = renderIndex(ofInstance: id), i < instanceTransforms.count else {
            return matrix_identity_float4x4
        }
        return instanceTransforms[i]
    }

    /// A joint whose state the loop solver owns.
    func isPassiveJoint(_ id: String) -> Bool {
        (asmJoints.first { ($0["id"] as? String) == id }?["passive"] as? Bool) ?? false
    }

    /// The driven joint of the closed mechanism `jid` belongs to: walk the
    /// joint graph through every instance except the ground, and return the
    /// first revolute/slider that is neither passive nor coupled.
    func mechanismDriver(from jid: String) -> String? {
        let ground = documentJSON.flatMap(AsmEdit.groundInstanceId)
        let joints = asmJoints
        func ends(_ j: [String: Any]) -> [String] {
            [j["parentInstanceId"] as? String, j["childInstanceId"] as? String].compactMap { $0 }
        }
        guard let start = joints.first(where: { ($0["id"] as? String) == jid }) else { return nil }
        var frontier = ends(start).filter { $0 != ground }
        var seenInst = Set(frontier), seenJoint = Set<String>()
        while !frontier.isEmpty {
            var next: [String] = []
            for j in joints {
                guard let id = j["id"] as? String, !seenJoint.contains(id),
                      ends(j).contains(where: frontier.contains) else { continue }
                seenJoint.insert(id)
                if let spec = jointSpec(id: id), spec.kind == .revolute || spec.kind == .slider,
                   !isPassiveJoint(id), jointCoupling(id) == nil { return id }
                for e in ends(j) where e != ground && seenInst.insert(e).inserted { next.append(e) }
            }
            frontier = next
        }
        return nil
    }

    // MARK: gesture

    /// Start a joint drag from a hit on instance `index` at kernel point `p`.
    /// Returns false when nothing jointed can be driven from there.
    @discardableResult
    func beginJointDrag(instance index: Int, at p: SIMD3<Float>,
                        ray: (o: SIMD3<Float>, d: SIMD3<Float>)) -> Bool {
        stopJointFlick(commit: true)
        guard !sim.isRunning,
              let jid = drivingJointId(forInstance: index),
              let spec = jointSpec(id: jid) else { return false }
        let parentW = worldTransform(ofInstance: spec.parentInstanceId)
        let a4 = parentW * SIMD4<Float>(Float(spec.parentAnchor.x), Float(spec.parentAnchor.y), Float(spec.parentAnchor.z), 1)
        let pivot = SIMD3(a4.x, a4.y, a4.z)
        let ax4 = parentW * SIMD4<Float>(Float(spec.axis.x), Float(spec.axis.y), Float(spec.axis.z), 0)
        let axis = simd_normalize(SIMD3(ax4.x, ax4.y, ax4.z))
        let root = rootDriver(of: jid)
        let values = currentJointValues()
        let isSlider = spec.kind == .slider
        let teeth = isSlider ? nil : gearTeeth(instance: index)
        // Tooth pitch in DRIVER degrees: every mesh in a train passes teeth
        // at the same rate, so ticking on the grabbed gear's pitch is right.
        let pitch = teeth.map { 360.0 / Double($0) / abs(root.ratio) }
        let value = values[root.id] ?? 0
        var s = JointDragSession(
            grabbedJointId: jid, driverJointId: root.id, chainRatio: root.ratio,
            isSlider: isSlider, pivot: pivot, axis: axis, anchor: p,
            lastVec: .zero, lastParam: 0, value: value,
            lastTime: Date.timeIntervalSinceReferenceDate,
            toothPitchDeg: pitch, lastTooth: pitch.map { Int(floor(value / $0)) } ?? 0)
        if isSlider {
            s.lastParam = Self.axisParam(ray: ray, pivot: pivot, axis: axis)
        } else {
            s.lastVec = Self.planeVector(p, pivot: pivot, axis: axis)
        }
        // A passive link (Strandbeest leg, four-bar coupler): drive the
        // mechanism's crank by search instead.
        if isPassiveJoint(jid) {
            guard let drv = mechanismDriver(from: jid), index < instanceTransforms.count else { return false }
            let r = rootDriver(of: drv)
            let local = instanceTransforms[index].inverse * SIMD4<Float>(p.x, p.y, p.z, 1)
            s = JointDragSession(
                grabbedJointId: drv, driverJointId: r.id, chainRatio: 1, isSlider: jointSpec(id: drv)?.kind == .slider,
                pivot: pivot, axis: axis, anchor: p, lastVec: .zero, lastParam: 0,
                value: values[r.id] ?? 0, lastTime: Date.timeIntervalSinceReferenceDate,
                toothPitchDeg: nil, lastTooth: 0, numeric: (index, local))
        }
        jointDrag = s
        pausePlayback()
        updateJointDragLabel()
        rippleConnections(from: index)
        return true
    }

    /// Follow the cursor ray (kernel space).
    func jointDragTo(ray: (o: SIMD3<Float>, d: SIMD3<Float>)) {
        guard var s = jointDrag else { return }
        let delta: Double
        if let n = s.numeric {
            // Angular miss of the grabbed point from the cursor ray, as a
            // function of the driver: one loop-closing FK solve per probe.
            func cost(_ v: Double) -> Float {
                var vals = currentJointValues()
                vals[s.driverJointId] = v
                guard let w = applyAssemblyPose(vals), n.instance < w.count else { return .infinity }
                let p4 = w[n.instance] * n.local
                let q = SIMD3(p4.x, p4.y, p4.z) - ray.o
                let depth = max(simd_dot(q, ray.d), 1e-3)
                return simd_length_squared(q - depth * ray.d) / (depth * depth)
            }
            var best = s.value, bestCost = cost(best)
            var step = s.isSlider ? 4.0 : 12.0
            var iter = 0
            while iter < 18, step > 0.01 {
                iter += 1
                var moved = false
                for cand in [best + step, best - step] {
                    let c = cost(cand)
                    if c < bestCost { best = cand; bestCost = c; moved = true; break }
                }
                if !moved { step /= 2 }
            }
            delta = best - s.value
            if let w = instanceTransforms.indices.contains(n.instance) ? instanceTransforms[n.instance] : nil {
                let p4 = w * n.local
                s.anchor = SIMD3(p4.x, p4.y, p4.z)
            }
            let now = Date.timeIntervalSinceReferenceDate
            let dt = max(now - s.lastTime, 1.0 / 240)
            s.velocity = 0.6 * (delta / dt) + 0.4 * s.velocity
            s.lastTime = now
            jointDrag = s
            setJointDragValue(s.value + delta)
            return
        }
        if s.isSlider {
            let t = Self.axisParam(ray: ray, pivot: s.pivot, axis: s.axis)
            delta = Double(t - s.lastParam)
            s.lastParam = t
            s.anchor += s.axis * Float(delta)
        } else {
            guard let hit = Self.planeHit(ray: ray, pivot: s.pivot, axis: s.axis) else { return }
            let v = Self.planeVector(hit, pivot: s.pivot, axis: s.axis)
            guard simd_length(v) > 1e-3, simd_length(s.lastVec) > 1e-3 else { s.lastVec = v; jointDrag = s; return }
            let sinA = simd_dot(simd_cross(s.lastVec, v), s.axis)
            let cosA = simd_dot(s.lastVec, v)
            delta = Double(atan2(sinA, cosA)) * 180 / .pi
            s.lastVec = v
            s.anchor = hit
        }
        let now = Date.timeIntervalSinceReferenceDate
        let dt = max(now - s.lastTime, 1.0 / 240)
        let dDriver = delta / s.chainRatio
        // Exponential smoothing so a jittery last frame doesn't set the flick.
        s.velocity = 0.6 * (dDriver / dt) + 0.4 * s.velocity
        s.lastTime = now
        jointDrag = s
        setJointDragValue(s.value + dDriver)
    }

    /// Release: flick if moving fast enough, otherwise commit now.
    func endJointDrag() {
        guard var s = jointDrag else { return }
        let idle = Date.timeIntervalSinceReferenceDate - s.lastTime
        if idle > 0.08 { s.velocity = 0 }              // held still before letting go
        let flickMin = s.isSlider ? 40.0 : 90.0          // mm/s or deg/s
        if abs(s.velocity) > flickMin {
            jointDrag = s
            startJointFlick()
        } else {
            jointDrag = nil
            finishJointDrag(s)
        }
    }

    // MARK: value, ticks, label

    private func setJointDragValue(_ raw: Double) {
        guard var s = jointDrag else { return }
        var v = raw
        if let l = jointSpec(id: s.driverJointId)?.limits {
            let clamped = min(max(v, l.lowerBound), l.upperBound)
            if clamped != v, clamped != s.value { hapticDetent(.levelChange) }
            v = clamped
        }
        s.value = v
        if let pitch = s.toothPitchDeg {
            let tooth = Int(floor(v / pitch))
            if tooth != s.lastTooth {
                s.lastTooth = tooth
                jointTick(speed: abs(s.velocity) * abs(s.chainRatio))
            }
        }
        jointDrag = s
        dragJoint(s.driverJointId, to: v)
        updateJointDragLabel()
    }

    /// One tooth passing: a trackpad detent and a soft click, rate-limited so
    /// a fast spin reads as a whir rather than a buzz.
    private func jointTick(speed: Double) {
        let now = Date.timeIntervalSinceReferenceDate
        guard now - jointTickLast > 0.018 else { return }
        jointTickLast = now
        hapticDetent(.alignment)
        chime.tick(speed: speed)
    }

    private func updateJointDragLabel() {
        guard let s = jointDrag else { return }
        let unit = s.isSlider ? " mm" : "°"
        let aff = couplingAffine(of: s.grabbedJointId)
        let grabbed = s.value * aff.r + aff.o
        let name = (asmJoints.first { ($0["id"] as? String) == s.grabbedJointId }?["name"] as? String) ?? s.grabbedJointId
        var text = "\(name)  \(String(format: "%.1f", grabbed))\(unit)"
        if s.driverJointId != s.grabbedJointId {
            text += "  ·  1:\(String(format: "%.2f", 1 / abs(s.chainRatio)))"
        }
        showManipLabel(text, at: s.anchor)
    }

    private func finishJointDrag(_ s: JointDragSession) {
        endRipple()
        // A gear train that just ran gets a real receipt. Snapshot it before
        // the commit: a commit rebuilds the document and re-mints joint ids.
        if s.toothPitchDeg != nil, gearTrain(of: s.driverJointId).count >= 2 {
            verifyGearTrain(from: s.driverJointId)
        }
        if timeline == nil { commitJointDrag(s.driverJointId) }
        fadeManipLabel()
    }

    // MARK: flick

    private func startJointFlick() {
        jointFlickTimer?.invalidate()
        let dt = 1.0 / 120
        jointFlickTimer = Timer.scheduledTimer(withTimeInterval: dt, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.stepJointFlick(dt) }
        }
    }

    private func stepJointFlick(_ dt: Double) {
        guard var s = jointDrag else { stopJointFlick(commit: false); return }
        // Bearing friction: bigger gears carry more inertia and coast longer.
        let decay = s.toothPitchDeg.map { 0.9 + 6.0 * $0 / 360 * 10 } ?? 2.2
        s.velocity *= exp(-dt * decay)
        let before = s.value
        jointDrag = s
        setJointDragValue(s.value + s.velocity * dt)
        guard let after = jointDrag?.value else { return }
        let stalled = abs(after - before) < 1e-6        // hit a limit
        if stalled || abs(s.velocity) < (s.isSlider ? 2 : 6) { stopJointFlick(commit: true) }
    }

    /// End a coast; commits the settled value as one undo step.
    func stopJointFlick(commit: Bool) {
        guard jointFlickTimer != nil else { return }
        jointFlickTimer?.invalidate()
        jointFlickTimer = nil
        guard let s = jointDrag else { return }
        jointDrag = nil
        if commit { finishJointDrag(s) }
    }

    // MARK: geometry

    /// Ray ∩ plane(pivot, axis); nil when the view is edge-on to the plane.
    static func planeHit(ray: (o: SIMD3<Float>, d: SIMD3<Float>), pivot: SIMD3<Float>,
                         axis: SIMD3<Float>) -> SIMD3<Float>? {
        let den = simd_dot(ray.d, axis)
        if abs(den) < 0.08 {
            // Edge-on: use the ray point nearest the pivot, flattened.
            let t = simd_dot(pivot - ray.o, ray.d)
            return ray.o + ray.d * t
        }
        let t = simd_dot(pivot - ray.o, axis) / den
        guard t > 0 else { return nil }
        return ray.o + ray.d * t
    }

    /// Component of `p − pivot` in the joint plane.
    static func planeVector(_ p: SIMD3<Float>, pivot: SIMD3<Float>, axis: SIMD3<Float>) -> SIMD3<Float> {
        let v = p - pivot
        return v - axis * simd_dot(v, axis)
    }

    /// Parameter along the axis line of the point closest to the ray.
    static func axisParam(ray: (o: SIMD3<Float>, d: SIMD3<Float>), pivot: SIMD3<Float>,
                          axis: SIMD3<Float>) -> Float {
        let w = pivot - ray.o
        let b = simd_dot(axis, ray.d)
        let den = 1 - b * b
        guard den > 1e-4 else { return 0 }
        return (b * simd_dot(w, ray.d) - simd_dot(w, axis)) / den
    }
}

// MARK: - headless smoke

/// `VCAD_OPEN=examples/gear-train.vcad VCAD_JOINTDRAG_SMOKE=/tmp/out` drives
/// the whole hero path with synthetic kernel rays — auto-mesh, a slow drag,
/// a flick, the clearance receipt — logging `[JOINTDRAG]` lines and saving
/// snapshots. Real mouse drags into a SwiftUI DragGesture aren't scriptable.
@MainActor
enum JointDragDemo {
    static func runIfRequested(_ m: EditorModel) async {
        guard let dir = ProcessInfo.processInfo.environment["VCAD_JOINTDRAG_SMOKE"] else { return }
        let url = URL(fileURLWithPath: dir)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        func log(_ s: String) { FileHandle.standardError.write(Data("[JOINTDRAG] \(s)\n".utf8)) }
        func wait(_ s: Double) async { try? await Task.sleep(nanoseconds: UInt64(s * 1e9)) }
        func snap(_ name: String) async {
            // An idle viewport doesn't render, and ARView only answers a
            // snapshot on a rendered frame: poke the pose channel first, and
            // never wait more than 3 s for the callback.
            m.playbackDirty = true
            m.poseTick &+= 1
            RenderGate.wake()
            await wait(0.6)
            guard let ar = ReleaseWindowController.shared.arView else { return }
            final class Flag { var done = false }
            let flag = Flag()
            ar.snapshot(saveToHDR: false) { img in
                if let img, let tiff = img.tiffRepresentation, let rep = NSBitmapImageRep(data: tiff),
                   let png = rep.representation(using: .png, properties: [:]) {
                    try? png.write(to: url.appendingPathComponent(name + ".png"))
                }
                flag.done = true
            }
            for _ in 0..<30 where !flag.done {
                m.poseTick &+= 1; RenderGate.wake()
                await wait(0.1)
            }
            let done = flag.done
            if !done { log("snapshot \(name) timed out") }
        }
        func gearAngles() -> [String] {
            (0..<m.instanceTransforms.count).compactMap { i in
                guard let z = m.gearSpec(instance: i)?.teeth else { return nil }
                let x = m.instanceTransforms[i].columns.0
                return "z\(z)=\(String(format: "%.2f", atan2(x.y, x.x) * 180 / .pi))°"
            }
        }
        var toasts: [String] = []
        func watchToasts(_ secs: Double) async {
            let end = Date().addingTimeInterval(secs)
            while Date() < end {
                if let t = GlassUI.shared.toast?.text, toasts.last != t { toasts.append(t); log("toast: \(t)") }
                await wait(0.05)
            }
        }

        for _ in 0..<300 where m.assemblyInstanceCount == 0 { await wait(0.1) }
        guard m.assemblyInstanceCount > 0 else { log("FAIL no assembly loaded"); return }
        // Exhibit mode (VCAD_EXHIBIT=1): snapshot at rest, then drive the
        // first non-passive revolute (the crank) a quarter and half turn.
        if ProcessInfo.processInfo.environment["VCAD_EXHIBIT"] != nil {
            if ProcessInfo.processInfo.environment["VCAD_PHOTOREAL"] != nil { m.realtimePhotoreal = true }
            await watchToasts(4)            // same settle the gear smoke uses
            log("exhibit: snapping rest (arView \(ReleaseWindowController.shared.arView != nil))")
            await snap("1-rest")
            log("exhibit: rest saved")
            if let crank = m.asmJoints.first(where: { j in
                guard let id = j["id"] as? String, let sp = m.jointSpec(id: id) else { return false }
                return sp.kind == .revolute && !m.isPassiveJoint(id) && m.jointCoupling(id) == nil
            })?["id"] as? String {
                let v0 = m.currentJointValues()[crank] ?? 0
                for (k, d) in [90.0, 200.0].enumerated() {
                    for t in 1...20 { m.dragJoint(crank, to: v0 + d * Double(t) / 20); await wait(0.02) }
                    await snap("\(k + 2)-crank+\(Int(d))")
                }
                log("exhibit snapshots done (crank \(crank))")
            }
            return
        }
        await watchToasts(4)
        let coupled = m.asmJoints.filter { $0["coupling"] != nil }
        log("instances=\(m.assemblyInstanceCount) couplings=\(coupled.count) " +
            coupled.map { "\(($0["name"] as? String) ?? "?") ← \((($0["coupling"] as? [String: Any])?["ratio"] as? NSNumber)?.doubleValue ?? 0)" }.joined(separator: ", "))
        log("angles at rest: \(gearAngles().joined(separator: " "))")
        await snap("1-meshed")

        // Grab the 16-tooth input gear 80% out on its +X, from straight above.
        guard let gi = (0..<m.instanceTransforms.count).first(where: { m.gearSpec(instance: $0)?.teeth == 16 }) else {
            log("FAIL no 16-tooth gear"); return
        }
        let c = m.instanceTransforms[gi].columns.3
        let center = SIMD3<Float>(c.x, c.y, c.z + 3)
        func ray(_ deg: Float) -> (o: SIMD3<Float>, d: SIMD3<Float>) {
            let r: Float = 12, a = deg * .pi / 180
            return (center + SIMD3(r * cos(a), r * sin(a), 300), SIMD3(0, 0, -1))
        }
        let before = gearAngles()
        let r0 = ray(0)
        guard m.beginJointDrag(instance: gi, at: center + SIMD3(12, 0, 0), ray: r0) else { log("FAIL beginJointDrag"); return }
        log("begin value=\(m.jointDrag?.value ?? .nan) driver=\(m.jointDrag?.driverJointId ?? "-") pivot=\(m.jointDrag?.pivot ?? .zero) ctr=\(center)")
        for k in 1...30 {
            m.jointDragTo(ray: ray(Float(k))); await wait(0.016)
            if k % 10 == 0 { log("  k=\(k) value=\(m.jointDrag?.value ?? .nan) \(gearAngles().joined(separator: " "))") }
        }
        log("slow drag 30°: \(before.joined(separator: " ")) → \(gearAngles().joined(separator: " "))")
        await wait(0.2)                       // held still → no flick
        m.endJointDrag()
        await watchToasts(8)
        await snap("2-dragged")

        // Flick: fast sweep, release while moving.
        let gi2 = (0..<m.instanceTransforms.count).first(where: { m.gearSpec(instance: $0)?.teeth == 16 }) ?? gi
        let c2 = m.instanceTransforms[gi2].columns.3
        let ctr2 = SIMD3<Float>(c2.x, c2.y, c2.z + 3)
        func ray2(_ deg: Float) -> (o: SIMD3<Float>, d: SIMD3<Float>) {
            let a = deg * .pi / 180
            return (ctr2 + SIMD3(12 * cos(a), 12 * sin(a), 300), SIMD3(0, 0, -1))
        }
        guard m.beginJointDrag(instance: gi2, at: ctr2 + SIMD3(12, 0, 0), ray: ray2(0)) else { log("FAIL flick begin"); return }
        for k in 1...8 { m.jointDragTo(ray: ray2(Float(k) * 12)); await wait(0.008) }
        let v = m.jointDrag?.velocity ?? 0
        m.endJointDrag()
        let flicking = m.jointFlickTimer != nil
        log("flick release velocity=\(Int(v))°/s coasting=\(flicking)")
        let t0 = Date()
        while m.jointFlickTimer != nil, Date().timeIntervalSince(t0) < 15 { await wait(0.05) }
        log("coast settled after \(String(format: "%.2f", Date().timeIntervalSince(t0)))s angles: \(gearAngles().joined(separator: " "))")
        await watchToasts(8)
        await snap("3-flicked")
        // Catch mid-coast, then re-grab right after release: the train must
        // continue from where it is (a flick commit re-mints ids).
        func inputIndex() -> Int? { (0..<m.instanceTransforms.count).first { m.gearSpec(instance: $0)?.teeth == 16 } }
        if let g = inputIndex() {
            let cc = m.instanceTransforms[g].columns.3, ctr = SIMD3<Float>(cc.x, cc.y, cc.z + 3)
            func r3(_ deg: Float) -> (o: SIMD3<Float>, d: SIMD3<Float>) {
                let a = deg * .pi / 180; return (ctr + SIMD3(12 * cos(a), 12 * sin(a), 300), SIMD3(0, 0, -1))
            }
            _ = m.beginJointDrag(instance: g, at: ctr + SIMD3(12, 0, 0), ray: r3(0))
            for k in 1...8 { m.jointDragTo(ray: r3(Float(k) * 12)); await wait(0.008) }
            m.endJointDrag()
            await wait(0.15)                                  // mid-coast
            let mid = gearAngles()
            let caught = m.jointFlickTimer != nil
            if let g2 = inputIndex() {
                let ok1 = m.beginJointDrag(instance: g2, at: ctr + SIMD3(12, 0, 0), ray: r3(0))
                let atGrab = gearAngles()
                for k in 1...10 { m.jointDragTo(ray: r3(Float(k))); await wait(0.016) }
                await wait(0.2); m.endJointDrag()
                let ok2 = m.beginJointDrag(instance: g2, at: ctr + SIMD3(12, 0, 0), ray: r3(0))
                let regrab = gearAngles()
                m.endJointDrag()
                log("catch mid-coast (was coasting \(caught)) begin=\(ok1): \(mid.joined(separator: " ")) → at grab \(atGrab.joined(separator: " ")); re-grab begin=\(ok2) at \(regrab.joined(separator: " "))")
            }
        }
        // The real gesture path: project the input gear's rim to the view and
        // hit-test it the way marqueeOrOrbitGesture does.
        if let g = inputIndex(), let centering = ReleaseWindowController.shared.centeringEntity,
           let proj = ReleaseWindowController.shared.projector(from: centering, camera: m.cameraPosition, lookAt: m.panOffset) {
            let w = m.instanceTransforms[g] * SIMD4<Float>(12, 0, 6, 1)
            if let vp = proj(SIMD3(w.x, w.y, w.z)) {
                let hit = ReleaseWindowController.shared.hits(atViewPoint: vp).first?.entity.name ?? "nothing"
                log("hit-test at input gear rim \(vp): first entity \(hit) (want inst\(g))")
            } else { log("hit-test: projection failed") }
        }
        let verdict = toasts.contains { $0.hasPrefix("Mesh verified") } ? "PASS" : "CHECK"
        log("\(verdict) done → \(dir)")
    }
}
