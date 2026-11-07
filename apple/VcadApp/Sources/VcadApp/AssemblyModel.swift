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
    var selectedInstanceId: String?
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

    /// Insert an instance of `partDefId` (offset so it doesn't land exactly on
    /// the last one). Selects it.
    @discardableResult
    func insertInstance(partDefId: String) -> String? {
        var id: String?
        let n = Double(asmInstances.count)
        guard assemblyEdit("Insert Instance", rebuild: true, {
            id = AsmEdit.insertInstance(&$0, partDefId: partDefId, translation: SIMD3(n * 20, 0, 0))
        }) else { return nil }
        assembly.selectedInstanceId = id
        return id
    }

    /// Insert an instance of a visible scene root, creating its part
    /// definition on first use (the web's Create Part Definition + Insert).
    @discardableResult
    func insertInstance(ofPart partIndex: Int) -> String? {
        guard let g = documentGraph, partIndex >= 0, partIndex < g.visibleRoots.count else { return nil }
        let root = g.visibleRoots[partIndex].nodeId
        var id: String?
        let n = Double(asmInstances.count)
        guard assemblyEdit("Insert Instance", rebuild: true, {
            let def = AsmEdit.ensurePartDef(&$0, rootNodeId: root, name: g.nodes[root]?.name)
            id = AsmEdit.insertInstance(&$0, partDefId: def, translation: SIMD3(n * 20, 0, 0))
        }) else { return nil }
        assembly.selectedInstanceId = id
        return id
    }

    func deleteInstance(_ id: String) {
        assemblyEdit("Delete Instance", rebuild: true) { AsmEdit.deleteInstance(&$0, id: id) }
        if assembly.selectedInstanceId == id { assembly.selectedInstanceId = nil }
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
        var id: String?
        guard assemblyEdit("Add Joint", rebuild: true, { id = AsmEdit.addJoint(&$0, spec) }) else {
            assembly.message = engineError
            return nil
        }
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
        v.merge(assembly.dragValues) { $1 }
        return v
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
