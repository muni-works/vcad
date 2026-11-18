import Foundation
import simd

// Gears that mesh themselves. When an assembly lands, any two SpurGear
// instances that sit on revolute joints with parallel axes, share a module,
// and whose centres are within a fraction of a module of the standard centre
// distance m(z1+z2)/2 are coupled: the driven joint gets
// `coupling = {driver, ratio: ∓z1/z2, offset}` with the offset putting tooth
// into gap (the kernel's `gear::mesh_rotation_deg` convention: tooth 0 on the
// gear's local +X). It lands as ONE named undo step with a toast, so ⌘Z
// un-meshes — and an un-meshed pair is not re-offered this session.
//
// Fails closed: anything ambiguous (axis mismatch, a driven gear already
// coupled elsewhere, module mismatch, off-distance) is simply not meshed.

/// One inferred gear mesh, ready to write.
struct GearMeshProposal: Equatable {
    let driverJoint: String
    let drivenJoint: String
    let driverTeeth: Int
    let drivenTeeth: Int
    let ratio: Double
    let offset: Double
    /// Unordered pair key by joint NAME (ids are re-minted on every edit, so
    /// an id key would re-offer a pair the user just un-meshed with ⌘Z).
    let key: String
}

extension EditorModel {
    /// Revolute joint driving instance `index` directly (not through welds).
    private func ownRevolute(_ index: Int) -> (id: String, spec: JointSpec)? {
        guard let iid = instanceID(at: index),
              let j = asmJoints.first(where: { ($0["childInstanceId"] as? String) == iid }),
              let id = j["id"] as? String, let spec = jointSpec(id: id),
              spec.kind == .revolute else { return nil }
        return (id, spec)
    }

    /// Every gear pair that should mesh but isn't coupled yet.
    func inferGearMeshes() -> [GearMeshProposal] {
        let n = min(assemblyInstanceCount, instanceTransforms.count)
        guard n >= 2 else { return [] }
        struct G { let i: Int; let joint: String; let z: Int; let m: Double
                   let center: SIMD3<Double>; let axis: SIMD3<Double>; let xAxis: SIMD3<Double>
                   let state: Double; let coupled: Bool }
        let values = currentJointValues()
        var gears: [G] = []
        for i in 0..<n {
            guard let (jid, spec) = ownRevolute(i), let g = gearSpec(instance: i) else { continue }
            let w = instanceTransforms[i]
            let c = SIMD3<Double>(Double(w.columns.3.x), Double(w.columns.3.y), Double(w.columns.3.z))
            let z = simd_normalize(SIMD3<Double>(Double(w.columns.2.x), Double(w.columns.2.y), Double(w.columns.2.z)))
            let x = SIMD3<Double>(Double(w.columns.0.x), Double(w.columns.0.y), Double(w.columns.0.z))
            // The joint must turn the gear about its own axis.
            let pw = worldTransform(ofInstance: spec.parentInstanceId)
            let a4 = pw * SIMD4<Float>(Float(spec.axis.x), Float(spec.axis.y), Float(spec.axis.z), 0)
            let ja = simd_normalize(SIMD3<Double>(Double(a4.x), Double(a4.y), Double(a4.z)))
            guard abs(simd_dot(ja, z)) > 0.999 else { continue }
            gears.append(G(i: i, joint: jid, z: g.teeth, m: g.module, center: c, axis: ja, xAxis: x,
                           state: values[jid] ?? spec.state, coupled: jointCoupling(jid) != nil))
        }
        var out: [GearMeshProposal] = []
        var drivenTaken = Set(gears.filter(\.coupled).map(\.joint))
        for a in 0..<gears.count {
            for b in (a + 1)..<gears.count {
                var g1 = gears[a], g2 = gears[b]
                guard abs(g1.m - g2.m) < 1e-6, abs(simd_dot(g1.axis, g2.axis)) > 0.999 else { continue }
                // Same plane, standard centre distance (within ¼ module).
                let d = g2.center - g1.center
                let along = simd_dot(d, g1.axis)
                let inPlane = d - g1.axis * along
                let want = g1.m * Double(g1.z + g2.z) / 2
                guard abs(simd_length(inPlane) - want) < 0.25 * g1.m else { continue }
                // Already in one train (directly or through others)? Meshing
                // them again would make a coupling cycle.
                if rootDriver(of: g1.joint).id == rootDriver(of: g2.joint).id { continue }
                // The driven side must be free; prefer driving the free one.
                if drivenTaken.contains(g2.joint) { swap(&g1, &g2) }
                guard !drivenTaken.contains(g2.joint) else { continue }

                // Phase: measure both gears' +X in g1's plane frame.
                let ax = g1.axis
                let ref = abs(ax.z) < 0.9 ? SIMD3<Double>(0, 0, 1) : SIMD3<Double>(1, 0, 0)
                let u = simd_normalize(simd_cross(ref, ax)), v = simd_cross(ax, u)
                func ang(_ p: SIMD3<Double>) -> Double { atan2(simd_dot(p, v), simd_dot(p, u)) * 180 / .pi }
                let dir = ang(g2.center - g1.center)
                let phi1 = ang(g1.xAxis), phi2 = ang(g2.xAxis)
                let sgn2: Double = simd_dot(g1.axis, g2.axis) > 0 ? 1 : -1
                let tau2 = 360.0 / Double(g2.z)
                let target = Self.meshRotationDeg(z1: g1.z, rot1: phi1, z2: g2.z, direction: dir)
                var corr = (target - phi2).truncatingRemainder(dividingBy: tau2)
                if corr > tau2 / 2 { corr -= tau2 }
                if corr < -tau2 / 2 { corr += tau2 }
                let k = Double(g1.z) / Double(g2.z)
                let ratio = -sgn2 * k
                let meshedState2 = g2.state + sgn2 * corr
                let nm = { (jid: String) in
                    (self.asmJoints.first { ($0["id"] as? String) == jid }?["name"] as? String) ?? jid
                }
                out.append(GearMeshProposal(driverJoint: g1.joint, drivenJoint: g2.joint,
                                            driverTeeth: g1.z, drivenTeeth: g2.z,
                                            ratio: ratio, offset: meshedState2 - ratio * g1.state,
                                            key: [nm(g1.joint), nm(g2.joint)].sorted().joined(separator: "|")))
                drivenTaken.insert(g2.joint)
            }
        }
        return out
    }

    /// Port of `vcad_ir::gear::mesh_rotation_deg`: tooth into gap.
    static func meshRotationDeg(z1: Int, rot1: Double, z2: Int, direction: Double) -> Double {
        let tau1 = 360.0 / Double(z1), tau2 = 360.0 / Double(z2)
        var f = ((direction - rot1) / tau1).truncatingRemainder(dividingBy: 1)
        if f < 0 { f += 1 }
        var r = (direction + 180 + (f - 0.5) * tau2).truncatingRemainder(dividingBy: 360)
        if r < 0 { r += 360 }
        return r
    }

    /// After an assembly lands: mesh what should mesh, once, as one undo step.
    func scheduleGearInference() {
        guard !gearInferencePending else { return }
        gearInferencePending = true
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.gearInferencePending = false
            guard self.timeline == nil, !self.sim.isRunning, !self.isLoonDocument else { return }
            let fresh = self.inferGearMeshes().filter { !self.gearMeshesOffered.contains($0.key) }
            guard !fresh.isEmpty else { return }
            fresh.forEach { self.gearMeshesOffered.insert($0.key) }
            let ok = self.applyGearMeshes(fresh)
            guard ok else { return }
            let label = fresh.map { "\($0.driverTeeth):\($0.drivenTeeth)" }.joined(separator: ", ")
            self.hapticDetent(.alignment)
            self.chime.play(.solved)
            GlassUI.shared.flash("Meshed \(label) — ⌘Z to unlink", symbol: "gearshape.2")
        }
    }

    /// Write proposals as couplings — ONE named undo step.
    @discardableResult
    func applyGearMeshes(_ fresh: [GearMeshProposal]) -> Bool {
        assemblyEdit(fresh.count == 1 ? "Mesh Gears" : "Mesh \(fresh.count) Gear Pairs", rebuild: true) { json in
            guard var joints = json["joints"] as? [[String: Any]] else { return }
            for p in fresh {
                guard let k = joints.firstIndex(where: { ($0["id"] as? String) == p.drivenJoint }) else { continue }
                joints[k]["coupling"] = ["driver": p.driverJoint, "ratio": p.ratio, "offset": p.offset]
            }
            json["joints"] = joints
        }
    }

    // MARK: receipt

    /// Joints in `driver`'s coupling train (its root plus everything that
    /// resolves to the same root).
    func gearTrain(of joint: String) -> [String] {
        let root = rootDriver(of: joint).id
        return asmJoints.compactMap { $0["id"] as? String }.filter { rootDriver(of: $0).id == root }
    }

    /// Real receipt for a meshed train: run the kernel clearance check at six
    /// poses across one tooth pitch of the root gear (mesh contact is periodic
    /// in the pitch) and report gear-vs-gear interference. Fails closed: an
    /// error or a missing pair reads as unverified, never as a pass.
    /// Overlap below this (mm³) between meshing teeth reads as contact.
    nonisolated static let meshContactToleranceMM3 = 0.05

    /// `gearNames` maps each moving instance name to its arbor (driving
    /// joint); parts welded to one arbor touch by design and aren't scored.
    nonisolated static func evaluateMeshReceipt(docs: [Data], gearNames: [String: String], pairs: [[Int]]? = nil)
        -> (ok: Bool, failed: Bool, checked: Int, clashes: Int, minGap: Double) {
        var minGap = Double.infinity, clashes = 0, checked = 0, failed = false
        for d in docs {
            guard let r = try? AnalysisFFI.clearance(doc: d, includeMesh: false, pairs: pairs) else { failed = true; break }
            for p in r.pairs {
                guard let ga = gearNames[p.a_name], let gb = gearNames[p.b_name], ga != gb else { continue }
                checked += 1
                // Zero-backlash teeth touch along the line of action; a
                // contact-level overlap is a mesh, not a clash. A real phase
                // error buries a tooth (mm³, not a sliver).
                if p.intersecting && p.overlap_volume_mm3 > Self.meshContactToleranceMM3 {
                    clashes += 1
                } else {
                    minGap = min(minGap, p.intersecting ? 0 : p.distance_mm)
                }
            }
        }
        return (!failed && checked > 0 && clashes == 0, failed, checked, clashes, minGap)
    }

    /// Docs posed across one tooth pitch, the gear instance names, and a
    /// "16:28:12" label — the inputs to `evaluateMeshReceipt`.
    func meshReceiptInputs(from joint: String) -> (docs: [Data], gearNames: [String: String], ratioText: String)? {
        guard let r = meshReceiptInputsWithPairs(from: joint) else { return nil }
        return (r.docs, r.gearNames, r.ratioText)
    }

    /// Only parts on DIFFERENT arbors can clash; asking the kernel about
    /// same-arbor pairs (a gear on its own shaft) made it compute overlap
    /// volumes for parts that touch by design — seconds of mesh booleans per
    /// pose, enough to choke the app after every flick.
    func meshReceiptInputsWithPairs(from joint: String) -> (docs: [Data], gearNames: [String: String], ratioText: String, pairs: [[Int]])? {
        let train = gearTrain(of: joint)
        guard train.count >= 2, var json = documentJSON,
              var joints = json["joints"] as? [[String: Any]] else { return nil }
        let root = rootDriver(of: joint).id
        guard let rk = joints.firstIndex(where: { ($0["id"] as? String) == root }) else { return nil }
        let instName: (String) -> String? = { iid in
            self.asmInstances.first { ($0["id"] as? String) == iid }
                .map { ($0["name"] as? String) ?? iid }
        }
        // Every instance the train moves: arbors and anything welded to them
        // (compound gears are separate instances on Fixed joints).
        let trainSet = Set(train)
        var gearNames: [String: String] = [:]
        for i in 0..<assemblyInstanceIDs.count {
            guard let dj = drivingJointId(forInstance: i), trainSet.contains(dj),
                  let name = instanceID(at: i).flatMap(instName) else { continue }
            gearNames[name] = dj
        }
        let rootChild = joints[rk]["childInstanceId"] as? String
        let rootGears = (0..<assemblyInstanceIDs.count).filter { drivingJointId(forInstance: $0) == root }
        let teeth = rootChild.flatMap(renderIndex(ofInstance:)).flatMap(gearSpec(instance:))?.teeth
            ?? rootGears.compactMap { gearSpec(instance: $0)?.teeth }.min() ?? 12
        let base = currentJointValues()[root] ?? 0
        var docs: [Data] = []
        for k in 0..<3 {
            joints[rk]["state"] = base + 360.0 / Double(teeth) * Double(k) / 3
            json["joints"] = joints
            if let d = try? JSONSerialization.data(withJSONObject: json) { docs.append(d) }
        }
        let ratioText = (0..<assemblyInstanceIDs.count).filter { i in
            drivingJointId(forInstance: i).map(trainSet.contains) ?? false
        }.compactMap { gearSpec(instance: $0)?.teeth }.map(String.init).joined(separator: ":")
        var arborOf: [Int: String] = [:]
        for i in 0..<assemblyInstanceIDs.count {
            if let dj = drivingJointId(forInstance: i), trainSet.contains(dj) { arborOf[i] = dj }
        }
        let idx = arborOf.keys.sorted()
        var pairs: [[Int]] = []
        for a in 0..<idx.count { for b in (a + 1)..<idx.count where arborOf[idx[a]] != arborOf[idx[b]] { pairs.append([idx[a], idx[b]]) } }
        return (docs, gearNames, ratioText, pairs)
    }

    func verifyGearTrain(from joint: String) {
        // One receipt at a time, and only once the hand has been still for a
        // moment: each pose re-evaluates the document, and a burst of flicks
        // used to stack up several of them and starve the app.
        let pres = MechanismPresentation.shared
        pres.receiptToken &+= 1
        let token = pres.receiptToken
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 1_200_000_000)
            guard let self, pres.receiptToken == token, !pres.receiptRunning, self.jointDrag == nil else { return }
            self.runGearReceipt(from: joint)
        }
    }

    private func runGearReceipt(from joint: String) {
        guard let (docs, gearNames, ratioText, pairs) = meshReceiptInputsWithPairs(from: joint) else { return }
        let pres = MechanismPresentation.shared
        pres.receiptRunning = true
        Task.detached(priority: .utility) {
            let r = Self.evaluateMeshReceipt(docs: docs, gearNames: gearNames, pairs: pairs)
            let (ok, failed, checked, clashes, minGap) = (r.ok, r.failed, r.checked, r.clashes, r.minGap)
            await MainActor.run {
                MechanismPresentation.shared.receiptRunning = false
                if ok {
                    self.chime.play(.solved)
                    GlassUI.shared.flash("Mesh verified \(ratioText) · 0 interferences · min gap \(String(format: "%.2f", minGap)) mm",
                                         symbol: "checkmark.seal")
                } else if failed || checked == 0 {
                    GlassUI.shared.flash("Mesh not verified — clearance check unavailable", symbol: "questionmark.circle")
                } else {
                    self.chime.play(.failed)
                    GlassUI.shared.flash("Mesh interferes \(ratioText) · \(clashes) clash\(clashes == 1 ? "" : "es") over a tooth pitch",
                                         symbol: "exclamationmark.triangle")
                }
            }
        }
    }
}
