import XCTest
import simd
@testable import VcadApp

/// Assembly / robotics parity gate (vcad 1.0 M4b): import a URDF robot, drag a
/// joint via FK, author a 3-key animation, save → reopen identical, run the
/// sim — plus insert-instance / joint / mate / keyframe authoring on a small
/// part document. Drives `EditorModel` the way the panels do.
@MainActor
final class AssemblyAuthoringTests: XCTestCase {
    private var temps: [URL] = []

    override func tearDown() async throws {
        for u in temps { try? FileManager.default.removeItem(at: u) }
    }

    private static var repoRoot: URL {
        var dir = URL(fileURLWithPath: #filePath)
        for _ in 0..<8 {
            dir = dir.deletingLastPathComponent()
            if FileManager.default.fileExists(atPath: dir.appendingPathComponent("third_party/booster-k1").path) {
                return dir
            }
        }
        XCTFail("could not locate the repo root from \(#filePath)")
        return dir
    }

    private func temp(_ ext: String = "vcad") -> URL {
        let u = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + "." + ext)
        temps.append(u)
        return u
    }

    private func waitUntil(_ what: String, timeout: TimeInterval = 20, _ cond: () -> Bool) {
        let deadline = Date().addingTimeInterval(timeout)
        while !cond() && Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        XCTAssertTrue(cond(), "timed out waiting for \(what)")
    }

    private func canonical(_ any: Any?) -> String {
        guard let any, JSONSerialization.isValidJSONObject(any),
              let d = try? JSONSerialization.data(withJSONObject: any, options: [.sortedKeys]) else {
            return String(describing: any)
        }
        return String(decoding: d, as: UTF8.self)
    }

    private func translation(_ m: float4x4) -> SIMD3<Float> { SIMD3(m.columns.3.x, m.columns.3.y, m.columns.3.z) }

    // MARK: the gate — K1 humanoid

    func testK1ImportFKDragThreeKeyAnimationSaveReopenAndSimulate() throws {
        let urdf = Self.repoRoot.appendingPathComponent("third_party/booster-k1/K1_22dof_floating.urdf")
        let m = EditorModel()
        let imported = try XCTUnwrap(m.importForeign(urdf), m.loadError ?? "URDF import failed")
        temps.append(imported)
        m.openDocument(imported)
        XCTAssertTrue(m.canEditDocument, m.engineError ?? "no engine")
        _ = m.buildScene()
        XCTAssertGreaterThan(m.assemblyInstanceCount, 20, "K1 has a link instance per URDF link")

        // Pick a driven revolute joint with a child instance.
        let joints = m.asmJoints.map(AsmEdit.jointSpec)
        let ids = m.asmJoints.compactMap { $0["id"] as? String }
        let idx = try XCTUnwrap(joints.firstIndex { $0.kind == .revolute && $0.childInstanceId.contains("Knee") }
                                ?? joints.firstIndex { $0.kind == .revolute })
        let jointId = ids[idx], child = joints[idx].childInstanceId
        let childIndex = try XCTUnwrap(m.assemblyInstanceIDs.firstIndex(of: child))

        // FK drag: the child link moves, the document doesn't (until commit).
        let rest = try XCTUnwrap(m.applyAssemblyPose(m.currentJointValues()))
        let range = joints[idx].limits ?? -90...90
        let target = min(max(30.0, range.lowerBound), range.upperBound) == 0 ? range.upperBound * 0.5
            : min(max(30.0, range.lowerBound), range.upperBound)
        m.dragJoint(jointId, to: target)
        let dragged = m.instanceTransforms
        XCTAssertEqual(dragged.count, rest.count)
        let moved = simd_length(dragged[childIndex].columns.0 - rest[childIndex].columns.0)
            + simd_length(dragged[childIndex].columns.2 - rest[childIndex].columns.2)
        XCTAssertGreaterThan(moved, 1e-3, "dragging \(jointId) must re-pose its child \(child)")
        XCTAssertEqual(m.jointSpec(id: jointId)?.state ?? .nan, joints[idx].state, accuracy: 1e-9,
                       "a drag is a preview")
        m.commitJointDrag(jointId)
        XCTAssertEqual(m.jointSpec(id: jointId)?.state ?? .nan, target, accuracy: 1e-9)
        XCTAssertEqual(m.undoActionName, "Move Joint")

        // Three keys: 0 s → 0, 1 s → target, 2 s → −target/2 (clamped).
        let third = min(max(-target / 2, range.lowerBound), range.upperBound)
        for (t, v) in [(0.0, 0.0), (1.0, target), (2.0, third)] {
            m.setPlaybackTime(t)
            m.dragJoint(jointId, to: v)
            m.keyJoint(jointId, at: t)
        }
        let keys = m.keys(forJoint: jointId)
        XCTAssertEqual(keys.map(\.t), [0, 1, 2])
        XCTAssertEqual(keys.map(\.value), [0, target, third])
        XCTAssertEqual(m.timeline?.durationS ?? 0, 2, accuracy: 1e-9, "the timeline grows to cover the last key")
        // Playback interpolates between keys.
        XCTAssertEqual(m.timeline?.jointValues(at: 0.5)[jointId] ?? .nan, target / 2, accuracy: 1e-9)
        // Move + delete + re-add round trip (key editing, not just appending).
        m.moveKey(jointId: jointId, from: 2, to: 1.5)
        XCTAssertEqual(m.keys(forJoint: jointId).map(\.t), [0, 1, 1.5])
        m.moveKey(jointId: jointId, from: 1.5, to: 2)
        m.deleteKey(jointId: jointId, t: 1)
        XCTAssertEqual(m.keys(forJoint: jointId).count, 2)
        m.undo()
        XCTAssertEqual(m.keys(forJoint: jointId).map(\.value), [0, target, third], "undo restores the key")

        // Save → reopen identical.
        let out = temp()
        m.saveDocumentAs(out)
        let saved = try XCTUnwrap(m.documentJSON)
        let r = EditorModel()
        r.openDocument(out)
        XCTAssertTrue(r.canEditDocument, r.engineError ?? "reopen failed")
        let back = try XCTUnwrap(r.documentJSON)
        // The engine re-keys part defs and instances on load; match them by
        // name (the M1 round-trip canonical form). Joint ids and the timeline
        // are stable and compared raw.
        XCTAssertEqual(WebRoundTripTests.canonical(back), WebRoundTripTests.canonical(saved),
                       "assembly must survive save → reopen")
        for k in ["joints", "timeline"] {
            XCTAssertEqual(canonical(back[k]), canonical(saved[k]), "\(k) must survive save → reopen")
        }
        XCTAssertEqual(r.keys(forJoint: jointId).map(\.value), [0, target, third])

        // Run the sim on the reopened robot.
        _ = r.buildScene()
        r.enableSimulation()
        waitUntil("the simulation to be ready") { r.sim.isAvailable && r.sim.isReady }
        r.sim.stepOnce()
        waitUntil("one control step") { r.sim.latest.step >= 1 }
        r.sim.teardown()
    }

    // MARK: authoring on a part document

    private func partDoc() throws -> URL {
        let doc: [String: Any] = [
            "version": "0.1",
            "nodes": [
                "1": ["id": 1, "name": "Base", "op": ["type": "Cube", "size": ["x": 40, "y": 40, "z": 10]]],
                "2": ["id": 2, "name": "Arm", "op": ["type": "Cube", "size": ["x": 10, "y": 10, "z": 60]]],
            ],
            "materials": [:], "part_materials": [:],
            "roots": [["root": 1, "material": "aluminum"], ["root": 2, "material": "steel"]],
        ]
        let url = temp()
        try JSONSerialization.data(withJSONObject: doc).write(to: url)
        return url
    }

    func testInsertInstanceJointsMatesAndCascadeDelete() throws {
        let m = EditorModel()
        m.openDocument(try partDoc())
        XCTAssertTrue(m.canEditDocument, m.engineError ?? "no engine")

        // Instance ids are re-keyed by every engine reload; hold them by name.
        func I(_ name: String) -> String { m.instanceId(named: name) ?? "missing:\(name)" }
        XCTAssertNotNil(m.insertInstance(ofPart: 0))
        XCTAssertNotNil(m.insertInstance(ofPart: 1))
        XCTAssertEqual(AsmEdit.partDefs(m.asmJSON).count, 2, "each part gets a definition on first insert")
        XCTAssertNotNil(m.insertInstance(ofPart: 1))
        XCTAssertEqual(Set(m.asmInstances.compactMap { $0["name"] as? String }), ["Base 1", "Arm 1", "Arm 2"])
        XCTAssertEqual(AsmEdit.partDefs(m.asmJSON).count, 2, "a second insert reuses the definition")
        XCTAssertEqual(m.asmInstances.count, 3)
        _ = m.buildScene()
        XCTAssertEqual(m.assemblyInstanceCount, 3)

        m.setGroundInstance(I("Base 1"))
        XCTAssertEqual(AsmEdit.groundInstanceId(m.asmJSON), I("Base 1"))

        // All five kinds, with anchors.
        var spec = JointSpec()
        spec.kind = .revolute
        spec.parentInstanceId = I("Base 1")
        spec.childInstanceId = I("Arm 1")
        spec.parentAnchor = SIMD3(20, 20, 10)
        spec.childAnchor = SIMD3(5, 5, 0)
        spec.axis = SIMD3(1, 0, 0)
        spec.limits = -90...90
        let j = try XCTUnwrap(m.addJoint(spec), m.assembly.message ?? "")
        let stored = try XCTUnwrap(m.jointSpec(id: j))
        XCTAssertEqual(stored.parentAnchor, spec.parentAnchor)
        XCTAssertEqual(stored.childAnchor, spec.childAnchor)
        XCTAssertEqual(stored.limits, -90...90)

        // FK: rotating 90° about X swings the arm's +Z into −Y (anchored at the pivot, not the world origin).
        _ = m.buildScene()
        let armIndex = try XCTUnwrap(m.assemblyInstanceIDs.firstIndex(of: I("Arm 1")))
        m.dragJoint(j, to: 90)
        let w = m.instanceTransforms[armIndex]
        let up = SIMD3(w.columns.2.x, w.columns.2.y, w.columns.2.z)
        XCTAssertEqual(up.y, -1, accuracy: 1e-4, "child +Z → −Y after +90° about X")
        let pivot = w * SIMD4<Float>(5, 5, 0, 1)
        XCTAssertEqual(pivot.x, 20, accuracy: 1e-3); XCTAssertEqual(pivot.y, 20, accuracy: 1e-3)
        XCTAssertEqual(pivot.z, 10, accuracy: 1e-3, "the child anchor stays on the parent anchor")
        m.dragJoint(j, to: 500)
        XCTAssertEqual(m.assembly.dragValues[j], 90, "drags clamp to the limits")
        m.assembly.dragValues = [:]

        // Validation: a second joint placing the same child, a loop, self-joint.
        var dup = spec; dup.parentInstanceId = I("Arm 2"); dup.childInstanceId = I("Arm 1")
        XCTAssertNil(m.addJoint(dup)); XCTAssertNotNil(m.assembly.message)
        var loop = JointSpec(); loop.kind = .fixed; loop.parentInstanceId = I("Arm 1"); loop.childInstanceId = I("Base 1")
        XCTAssertNil(m.addJoint(loop), "a kinematic loop is refused")
        var back = JointSpec(); back.kind = .ball; back.parentInstanceId = I("Arm 2"); back.childInstanceId = I("Arm 2")
        XCTAssertNil(m.addJoint(back))

        // Every kind serializes as the kernel's JointKind and edits keep the id.
        for kind in JointKindChoice.allCases {
            var e = try XCTUnwrap(m.jointSpec(id: j)); e.kind = kind
            XCTAssertTrue(m.updateJoint(j, e), m.assembly.message ?? "")
            let kj = (m.asmJoints.first { $0["id"] as? String == j }?["kind"] as? [String: Any])
            XCTAssertEqual(kj?["type"] as? String, kind.rawValue)
            XCTAssertEqual(kj?["axis"] != nil, kind.hasAxis)
        }

        // Mates: planar offset measured from the pose.
        var mate = MateSpec()
        mate.kind = .planarOffset
        mate.instanceA = I("Base 1"); mate.instanceB = I("Arm 2")
        mate.axis = SIMD3(1, 0, 0)
        mate.offset = 40                          // Arm 2 was inserted at x = 2 · 20
        let mid = try XCTUnwrap(m.addMate(mate))
        XCTAssertEqual(m.assembly.mateVerdicts.first { $0.id == mid }?.pass, true,
                       m.assembly.mateVerdicts.first?.detail ?? m.assembly.mateError ?? "")
        m.setInstanceTranslation(I("Arm 2"), SIMD3(55, 0, 0))
        m.checkMates()
        let v = try XCTUnwrap(m.assembly.mateVerdicts.first { $0.id == mid })
        XCTAssertFalse(v.pass)
        XCTAssertEqual(v.measured, 55, accuracy: 1e-6)

        // Cascade delete: joints, mates and joint tracks naming the instance go too.
        m.keyJoint(j, at: 0.5)
        XCTAssertNotNil(m.timeline)
        XCTAssertEqual(m.asmJoints.count, 1)
        m.deleteInstance(I("Arm 1"))
        XCTAssertTrue(m.asmJoints.isEmpty, "the joint placing the deleted instance goes with it")
        XCTAssertNil(m.timeline, "the only track went with its joint")
        m.deleteInstance(I("Base 1"))
        XCTAssertNil(AsmEdit.groundInstanceId(m.asmJSON))
        XCTAssertTrue(m.asmMates.isEmpty)
        m.undo(); m.undo()
        XCTAssertEqual(m.asmInstances.count, 3, "undo restores both instances")
        XCTAssertEqual(m.asmMates.count, 1)
    }

    func testTimelineNormalizeIsTheKernelCanonicalForm() {
        let tl: [String: Any] = [
            "durationS": 1.0, "fps": 24.0,
            "tracks": [["target": ["type": "Joint", "jointId": "j"],
                        "keys": [["t": 0.8, "value": 2.0], ["t": 0.2, "value": 1.0, "ease": "linear"],
                                 ["t": 5.0, "value": 9.0]]]],
        ]
        let n = AsmEdit.normalized(tl)
        let keys = ((n?["tracks"] as? [[String: Any]])?.first?["keys"] as? [[String: Any]]) ?? []
        XCTAssertEqual(keys.compactMap { ($0["t"] as? NSNumber)?.doubleValue }, [0.2, 0.8, 1.0])
        XCTAssertNil(keys.first?["ease"], "linear is elided")
        XCTAssertNil(AsmEdit.normalized(["durationS": 0, "fps": 24, "tracks": []]), "zero duration is refused")
    }
}
