import CVcadFFI
import Foundation
import Observation
import simd

// Assembly / robotics authoring on the editor model: insert instances, add and
// edit joints (picked anchors), mates, ground, FK drag, and keyframes. Every
// document change is one undoable IR edit through `assemblyEdit` (the engine
// reloads the edited IR); FK drags preview through the kernel solver
// (`vcad_scene_solve_fk`) without touching the document until committed.

/// UI state for the assembly panel and the keyframe editor.
@Observable
final class AssemblyUIState {
    var showsPanel = false
    var showsTimeline = false
    var selectedJointId: String?
    /// Selected instance, held by NAME: the engine re-keys instance ids on
    /// every reload, so an id doesn't survive the next edit.
    var selectedInstanceName: String?
    /// Keyframe selection in the timeline editor.
    var selectedKey: (jointId: String, t: Double)? {
        didSet { if let k = selectedKey { selectedJointId = k.jointId } }
    }
    /// Live FK-drag values (joint id → value) not yet committed.
    var dragValues: [String: Double] = [:]
    /// Last mate check.
    var mateVerdicts: [MateVerdict] = []
    var mateError: String?
    /// Last authoring refusal (validation), shown inline.
    var message: String?
    /// Video export progress (0…1) while exporting.
    var exportProgress: Double?
}

extension EditorModel {

    // MARK: reads

    var asmJSON: [String: Any] { documentJSON ?? [:] }
    /// Whether the document is (or can become) an assembly.
    var isAssemblyDocument: Bool {
        assemblyInstanceCount > 0 || !AsmEdit.instances(asmJSON).isEmpty || !AsmEdit.partDefs(asmJSON).isEmpty
    }
    var asmInstances: [[String: Any]] { AsmEdit.instances(asmJSON) }
    var asmJoints: [[String: Any]] { AsmEdit.joints(asmJSON) }
    var asmMates: [[String: Any]] { AsmEdit.mates(asmJSON) }

    func jointSpec(id: String) -> JointSpec? {
        asmJoints.first { $0["id"] as? String == id }.map(AsmEdit.jointSpec)
    }

    // MARK: instances

    /// Current id of the instance named `name` (ids don't survive an edit).
    func instanceId(named name: String) -> String? {
        asmInstances.first { $0["name"] as? String == name }?["id"] as? String
    }
    var selectedInstanceId: String? { assembly.selectedInstanceName.flatMap(instanceId(named:)) }

    /// Insert an instance of `partDefId` (offset so it doesn't land exactly on
    /// the last one). Selects it.
    @discardableResult
    func insertInstance(partDefId: String) -> String? {
        let n = Double(asmInstances.count)
        let id = newInstanceId {
            assemblyEdit("Insert Instance", rebuild: true) {
                AsmEdit.insertInstance(&$0, partDefId: partDefId, translation: SIMD3(n * 20, 0, 0))
            }
        }
        assembly.selectedInstanceName = id.map { AsmEdit.instanceName(asmJSON, $0) }
        return id
    }

    /// Bodies a clearance check measures — what the viewport draws: every
    /// assembly instance plus the visible roots no part def owns, or just the
    /// visible roots when there is no assembly (mirrors `vcad_clearance`).
    var clearanceBodyCount: Int {
        guard let json = documentJSON else { return 0 }
        let roots = ((json["roots"] as? [[String: Any]]) ?? []).filter { ($0["visible"] as? Bool) != false }
        let instances = (json["instances"] as? [Any])?.count ?? 0
        guard instances > 0 else { return roots.count }
        let defRoots = Set(((json["partDefs"] as? [String: [String: Any]]) ?? [:]).values
            .compactMap { ($0["root"] as? NSNumber)?.intValue })
        return instances + roots.filter { !defRoots.contains(($0["root"] as? NSNumber)?.intValue ?? -1) }.count
    }

    /// Open Analyze ▸ Clearance & Interference and run it — the assembly
    /// dock's check button. Also re-checks mates when there are any.
    func checkInterference() {
        let s = AnalysisStore.shared
        s.shown = true; s.tab = .clearance; s.overlayRevision += 1
        if !asmMates.isEmpty { assembly.showsPanel = true; checkMates() }
        Task { await s.runClearance(self) }
    }

    /// What a visible scene root is called wherever a part is listed: its
    /// Model-tree name (the feature under the placement wrappers — "Box",
    /// not the unnamed Translate a click-placed primitive's root is).
    func partDisplayName(_ partIndex: Int) -> String {
        if let n = featureNodes.first(where: { $0.partIndex == partIndex })?.name, !n.isEmpty { return n }
        if let g = documentGraph, partIndex >= 0, partIndex < g.visibleRoots.count, let json = documentJSON,
           let n = AsmEdit.partName(json, rootNodeId: g.visibleRoots[partIndex].nodeId) { return n }
        return "Part \(partIndex + 1)"
    }

    /// Insert an instance of a visible scene root, creating its part
    /// definition on first use (the web's Create Part Definition + Insert).
    @discardableResult
    func insertInstance(ofPart partIndex: Int) -> String? {
        guard let g = documentGraph, partIndex >= 0, partIndex < g.visibleRoots.count else { return nil }
        let root = g.visibleRoots[partIndex].nodeId
        let n = Double(asmInstances.count)
        let id = newInstanceId {
            assemblyEdit("Insert Instance", rebuild: true) {
                let def = AsmEdit.ensurePartDef(&$0, rootNodeId: root,
                                                 name: AsmEdit.partName($0, rootNodeId: root) ?? partDisplayName(partIndex))
                AsmEdit.insertInstance(&$0, partDefId: def, translation: SIMD3(n * 20, 0, 0))
            }
        }
        assembly.selectedInstanceName = id.map { AsmEdit.instanceName(asmJSON, $0) }
        return id
    }

    /// The engine re-keys instances (and part defs) when it reloads the IR,
    /// so the id an edit wrote is not the id it ends up with: the new
    /// instance is the one id that wasn't there before.
    private func newInstanceId(_ edit: () -> Bool) -> String? {
        let before = Set(asmInstances.compactMap { $0["id"] as? String })
        guard edit() else { return nil }
        return asmInstances.compactMap { $0["id"] as? String }.first { !before.contains($0) }
    }

    func deleteInstance(_ id: String) {
        if assembly.selectedInstanceName == AsmEdit.instanceName(asmJSON, id) { assembly.selectedInstanceName = nil }
        assemblyEdit("Delete Instance", rebuild: true) { AsmEdit.deleteInstance(&$0, id: id) }
    }

    func renameInstance(_ id: String, _ name: String) {
        let n = name.trimmingCharacters(in: .whitespaces)
        guard !n.isEmpty else { return }
        assemblyEdit("Rename Instance", rebuild: true) { AsmEdit.renameInstance(&$0, id: id, name: n) }
    }

    func setInstanceTranslation(_ id: String, _ t: SIMD3<Double>) {
        assemblyEdit("Move Instance", rebuild: true) { AsmEdit.setInstanceTranslation(&$0, id: id, t) }
    }

    func setGroundInstance(_ id: String?) {
        assemblyEdit("Set Ground", rebuild: true) { AsmEdit.setGround(&$0, id: id) }
    }

    // MARK: joints

    /// Add a joint; nil + `assembly.message` when it would be invalid.
    @discardableResult
    func addJoint(_ spec: JointSpec) -> String? {
        if let why = AsmEdit.validate(asmJSON, spec) { assembly.message = why; return nil }
        var name: String?
        guard assemblyEdit("Add Joint", rebuild: true, { name = AsmEdit.addJoint(&$0, spec) }) else {
            assembly.message = engineError
            return nil
        }
        // Resolve by name: that is the handle the engine keeps.
        let id = asmJoints.first { $0["name"] as? String == name }?["id"] as? String
        assembly.message = nil
        assembly.selectedJointId = id
        return id
    }

    @discardableResult
    func updateJoint(_ id: String, _ spec: JointSpec) -> Bool {
        if let why = AsmEdit.validate(asmJSON, spec, editing: id) { assembly.message = why; return false }
        let ok = assemblyEdit("Edit Joint", rebuild: true) { AsmEdit.updateJoint(&$0, id: id, spec) }
        assembly.message = ok ? nil : engineError
        return ok
    }

    func deleteJoint(_ id: String) {
        assemblyEdit("Delete Joint", rebuild: true) { AsmEdit.deleteJoint(&$0, id: id) }
        if assembly.selectedJointId == id { assembly.selectedJointId = nil }
        assembly.dragValues.removeValue(forKey: id)
    }

    /// Anchor from the current topology pick: the picked face / edge centre in
    /// its part's local frame — exactly the frame joint anchors live in. Also
    /// returns the part def that part is, so the caller can check it matches
    /// the instance being anchored.
    func pickedAnchor() -> (point: SIMD3<Double>, partDefId: String?)? {
        guard let ref = picking.selection.first,
              let topo = partTopology(ref.part) else { return nil }
        let p: SIMD3<Float>
        switch ref.kind {
        case .face: guard let f = topo.face(ref.id) else { return nil }; p = f.centerF
        case .edge: guard let e = topo.edge(ref.id) else { return nil }; p = e.midpoint
        }
        var defId: String?
        if let g = documentGraph, ref.part < g.visibleRoots.count {
            let root = g.visibleRoots[ref.part].nodeId
            defId = AsmEdit.partDefs(asmJSON).first { $0.root == root }?.id
        }
        return (SIMD3(Double(p.x), Double(p.y), Double(p.z)), defId)
    }

    // MARK: FK drag

    /// Joint values for a pose: authored state, then the timeline at the
    /// playhead, then live drag overrides.
    func currentJointValues() -> [String: Double] {
        var v: [String: Double] = [:]
        for j in asmJoints {
            guard let id = j["id"] as? String else { continue }
            v[id] = (j["state"] as? NSNumber)?.doubleValue ?? 0
        }
        if let tl = timeline { v.merge(tl.jointValues(at: playbackTime)) { $1 } }
        // The Pose panel's values (keyed by joint name, else id) are what's on
        // screen; without them the joint list read 0.0° while the arm sat at 77°.
        for j in asmJoints {
            guard let id = j["id"] as? String else { continue }
            let key = (j["name"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? id
            if let posed = jointPose[key] { v[id] = posed }
        }
        v.merge(assembly.dragValues) { $1 }
        return v
    }

    /// A pose set from outside the panel supersedes any drag left on those
    /// joints. A drag override outranks the pose, so one left behind pinned
    /// the Joints panel to a value the joint no longer has.
    func clearJointDrags(forPoseKeys keys: Set<String>) {
        guard !assembly.dragValues.isEmpty else { return }
        for j in asmJoints {
            guard let id = j["id"] as? String else { continue }
            let key = (j["name"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? id
            if keys.contains(key) || keys.contains(id) { assembly.dragValues.removeValue(forKey: id) }
        }
    }

    /// Live FK while dragging a joint: one kernel solve, no document change.
    func dragJoint(_ id: String, to value: Double) {
        var v = value
        if let l = jointSpec(id: id)?.limits { v = min(max(v, l.lowerBound), l.upperBound) }
        assembly.dragValues[id] = v
        pausePlayback()
        applyAssemblyPose(currentJointValues())
    }

    /// Commit a drag as the joint's authored state (one undo step).
    func commitJointDrag(_ id: String) {
        guard let v = assembly.dragValues.removeValue(forKey: id) else { return }
        assemblyEdit("Move Joint", rebuild: true) { AsmEdit.setJointState(&$0, id: id, v) }
    }

    /// Solve FK for `values` and publish the transforms (the playback channel).
    @discardableResult
    func applyAssemblyPose(_ values: [String: Double]) -> [float4x4]? {
        guard let scene = residentAssemblyScene, assemblyInstanceCount > 0,
              !values.isEmpty, let data = try? JSONSerialization.data(withJSONObject: values) else { return nil }
        var out = [Double](repeating: 0, count: assemblyInstanceCount * 16)
        let n = data.withUnsafeBytes { raw in
            vcad_scene_solve_fk(scene, raw.bindMemory(to: UInt8.self).baseAddress, data.count, &out, out.count)
        }
        guard n == assemblyInstanceCount else { return nil }
        let world = (0..<n).map { Self.mat4(out, at: $0 * 16) }
        instanceTransforms = world
        playbackDirty = true
        poseTick &+= 1
        return world
    }

    // MARK: mates

    @discardableResult
    func addMate(_ spec: MateSpec) -> String? {
        let ids = Set(asmInstances.compactMap { $0["id"] as? String })
        guard ids.contains(spec.instanceA), ids.contains(spec.instanceB), spec.instanceA != spec.instanceB else {
            assembly.message = "A mate needs two different instances."
            return nil
        }
        var id: String?
        guard assemblyEdit("Add Mate", rebuild: false, { id = AsmEdit.addMate(&$0, spec) }) else { return nil }
        checkMates()
        return id
    }

    func deleteMate(_ id: String) {
        assemblyEdit("Delete Mate", rebuild: false) { AsmEdit.deleteMate(&$0, id: id) }
        checkMates()
    }

    func checkMates() {
        switch AssemblyFFI.checkMates(asmJSON) {
        case .success(let v): assembly.mateVerdicts = v; assembly.mateError = nil
        case .failure(let e): assembly.mateVerdicts = []; assembly.mateError = e.message
        }
    }

    // MARK: keyframes

    /// Key the joint's current value (drag override, else the pose at the
    /// playhead) at the playhead.
    func keyJoint(_ id: String, at t: Double? = nil) {
        let time = t ?? playbackTime
        let value = currentJointValues()[id] ?? 0
        assemblyEdit("Add Keyframe", rebuild: false) { AsmEdit.setKey(&$0, jointId: id, t: time, value: value) }
        assembly.dragValues.removeValue(forKey: id)
        assembly.selectedKey = (id, time)
        setPlaybackTime(time)
    }

    /// Key every joint that is being dragged (or all driven joints when none
    /// is) at the playhead — "key pose".
    func keyPose() {
        let ids = assembly.dragValues.isEmpty
            ? asmJoints.filter { JointKindChoice(rawValue: (($0["kind"] as? [String: Any])?["type"] as? String) ?? "")?.hasLimits == true }
                .compactMap { $0["id"] as? String }
            : Array(assembly.dragValues.keys).sorted()
        guard !ids.isEmpty else { return }
        let values = currentJointValues(), time = playbackTime
        assemblyEdit("Key Pose", rebuild: false) { json in
            for id in ids { AsmEdit.setKey(&json, jointId: id, t: time, value: values[id] ?? 0) }
        }
        assembly.dragValues = [:]
        setPlaybackTime(time)
    }

    func setKeyValue(jointId: String, t: Double, value: Double, ease: String = "linear") {
        assemblyEdit("Edit Keyframe", rebuild: false) {
            AsmEdit.setKey(&$0, jointId: jointId, t: t, value: value, ease: ease)
        }
        setPlaybackTime(playbackTime)
    }

    func moveKey(jointId: String, from: Double, to: Double) {
        let dur = timeline?.durationS ?? AsmEdit.defaultDuration
        let dest = min(max(to, 0), dur)
        assemblyEdit("Move Keyframe", rebuild: false) { AsmEdit.moveKey(&$0, jointId: jointId, from: from, to: dest) }
        assembly.selectedKey = (jointId, dest)
        setPlaybackTime(playbackTime)
    }

    func setKeyEase(jointId: String, t: Double, ease: String) {
        assemblyEdit("Keyframe Easing", rebuild: false) { AsmEdit.setKeyEase(&$0, jointId: jointId, t: t, ease: ease) }
        setPlaybackTime(playbackTime)
    }

    func deleteKey(jointId: String, t: Double) {
        assemblyEdit("Delete Keyframe", rebuild: false) { AsmEdit.deleteKey(&$0, jointId: jointId, t: t) }
        assembly.selectedKey = nil
        if timeline != nil { setPlaybackTime(playbackTime) } else { applyAssemblyPose(currentJointValues()) }
    }

    func setTimelineDuration(_ d: Double) {
        assemblyEdit("Timeline Length", rebuild: false) { AsmEdit.setDuration(&$0, d) }
        setPlaybackTime(playbackTime)
    }

    /// Keys of the joint's track (t, value, ease), time order.
    func keys(forJoint id: String) -> [DocAnimKey] {
        timeline?.jointTracks.first { $0.jointId == id }?.keys ?? []
    }
}
