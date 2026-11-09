import XCTest
@testable import VcadApp

/// The live link to `@vcad/mcp`, driven exactly as the bridge dispatches it.
@MainActor
final class AgentLinkTests: XCTestCase {
    private static let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()

    override func setUp() async throws {
        GlassUI.shared.space = .design
    }

    private func call(_ m: EditorModel, _ name: String, _ args: [String: Any] = [:]) async throws -> [String: Any] {
        let r = try await AgentTools.call(name, args, model: m)
        let text = ((r["content"] as? [[String: Any]])?.first?["text"] as? String) ?? ""
        XCTAssertNil(r["isError"], "\(name) failed: \(text)")
        return (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any] ?? ["text": text]
    }

    private func box(_ x: Double) -> [String: Any] {
        ["version": "0.1",
         "nodes": ["1": ["id": 1, "name": "box", "op": ["type": "Cube", "size": ["x": x, "y": 10, "z": 10]]]],
         "materials": [String: Any](), "part_materials": [String: Any](),
         "roots": [["root": 1, "material": "default"]]]
    }

    private func arm() throws -> [String: Any] {
        let url = Self.repoRoot.appendingPathComponent("examples/robot-arm-2dof.vcad")
        return try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
    }

    private func adopt(_ m: EditorModel, _ doc: [String: Any], link: String, base: String? = nil) async throws -> [String: Any] {
        var args: [String: Any] = ["document": doc, "link_id": link, "tool": "create"]
        if let base { args["base_dir"] = base }
        return try await call(m, "adopt_document", args)
    }

    private func userEdit(_ m: EditorModel) async throws {
        _ = try await call(m, "create", ["type": "cube", "name": "Mine", "params": ["size": ["x": 5, "y": 5, "z": 5]]])
    }

    // MARK: defect 1 — one window, one session

    func testNewSessionTakesOverAnUneditedAgentPush() async throws {
        let m = EditorModel()
        let first = try await adopt(m, box(10), link: "a")
        XCTAssertEqual(first["adopted"] as? Bool, true)
        let second = try await adopt(m, box(20), link: "b")
        XCTAssertEqual(second["adopted"] as? Bool, true, "\(second)")
        XCTAssertEqual(m.agentLinkID, "b")
        XCTAssertEqual(m.undoStack.count, 2, "a takeover is one undo step, so it can be walked back")
    }

    func testUserEditsBlockTakeoverAndTheirOwnSessionIsToldEdited() async throws {
        let m = EditorModel()
        _ = try await adopt(m, box(10), link: "a")
        try await userEdit(m)
        let other = try await adopt(m, box(20), link: "b")
        XCTAssertEqual(other["reason"] as? String, "busy")
        let same = try await adopt(m, box(20), link: "a")
        XCTAssertEqual(same["reason"] as? String, "edited")
        XCTAssertEqual((m.documentJSON?["roots"] as? [Any])?.count, 2, "the user's part survives both")
    }

    func testPullingTheUsersEditsEndsTakeover() async throws {
        let m = EditorModel()
        _ = try await adopt(m, box(10), link: "a")
        try await userEdit(m)
        _ = try await call(m, "link_status", ["link_id": "a", "pull": true])
        let other = try await adopt(m, box(20), link: "b")
        XCTAssertEqual(other["reason"] as? String, "busy", "the revision matches again, but the document is now the user's")
    }

    func testUnlinkedUserWorkIsNeverOverwritten() async throws {
        let m = EditorModel()
        try await userEdit(m)
        let r = try await adopt(m, box(20), link: "b")
        XCTAssertEqual(r["reason"] as? String, "busy")
    }

    // MARK: defect 2 — pose while solving

    func testPoseSentBeforeTheSceneSolvesIsAppliedWhenItLands() async throws {
        let m = EditorModel()
        _ = try await adopt(m, try arm(), link: "a")
        XCTAssertEqual(m.assemblyInstanceCount, 0, "not solved yet")
        let name = EditorModel.poseKey(try XCTUnwrap(m.drivenJoints.first))
        _ = try await call(m, "set_pose", ["joints": [name: 10]])
        let held = try await call(m, "set_pose", ["joints": [name: 45]])
        XCTAssertTrue((held["text"] as? String)?.contains("held") == true, "\(held)")

        _ = m.buildScene()
        XCTAssertGreaterThan(m.assemblyInstanceCount, 0)
        XCTAssertEqual(m.jointPose[name], 45, "latest wins")
        XCTAssertNotEqual(m.instanceTransforms, m.authoredInstanceTransforms, "the held pose reached the scene")
    }

    func testPoseOnADocumentWithNoJointsStillRefuses() async throws {
        let m = EditorModel()
        _ = try await adopt(m, box(10), link: "a")
        do {
            _ = try await AgentTools.call("set_pose", ["joints": ["x": 1]], model: m)
            XCTFail("should refuse")
        } catch let f as AgentTools.Failure {
            XCTAssertTrue(f.message.contains("poseable"))
        }
    }

    // MARK: defect 3 — stale workspace

    func testAdoptingAMechanicalDocumentLeavesElectronicsOnceOnly() async throws {
        let m = EditorModel()
        GlassUI.shared.enter(.electronics, model: m)
        _ = try await adopt(m, box(10), link: "a")
        XCTAssertEqual(m.workspace, .design)
        XCTAssertEqual(GlassUI.shared.space, .design)

        GlassUI.shared.enter(.electronics, model: m)   // the user walks back
        _ = try await adopt(m, box(11), link: "a")
        XCTAssertEqual(m.workspace, .electronics, "a later push doesn't pull them out again")
    }

    func testPoseMovesALinkedWindowToAssemblyOnce() async throws {
        let m = EditorModel()
        _ = try await adopt(m, try arm(), link: "a")
        GlassUI.shared.enter(.drawing, model: m)
        let name = EditorModel.poseKey(try XCTUnwrap(m.drivenJoints.first))
        _ = try await call(m, "set_pose", ["joints": [name: 5]])
        XCTAssertEqual(GlassUI.shared.space, .assembly)

        GlassUI.shared.enter(.drawing, model: m)       // the user walks back
        _ = try await call(m, "set_pose", ["joints": [name: 6]])
        XCTAssertEqual(GlassUI.shared.space, .drawing, "a pose stream doesn't fight them")
    }

    func testPoseLeavesAnUnlinkedWindowAlone() throws {
        let m = EditorModel()
        GlassUI.shared.enter(.drawing, model: m)
        AgentEdits.surfacePose(m)
        XCTAssertEqual(GlassUI.shared.space, .drawing)
    }

    // MARK: defect 4 — a frozen mesh added to an assembly

    func testAMeshRootInAnAssemblyIsDrawnAndAPlainAssemblyHasNoExtras() async throws {
        let plain = EditorModel()
        _ = try await adopt(plain, try arm(), link: "a")
        XCTAssertEqual(plain.buildScene().instances.count, 4, "part-definition roots are not drawn twice")

        var doc = try arm()
        var nodes = try XCTUnwrap(doc["nodes"] as? [String: Any])
        nodes["900"] = ["id": 900, "name": "topopt",
                        "op": ["type": "ImportedMesh", "positions": [0, 0, 0, 90, 0, 0, 0, 90, 0, 0, 0, 90],
                               "indices": [0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3], "source": "topology_optimize"]] as [String: Any]
        doc["nodes"] = nodes
        doc["roots"] = (doc["roots"] as? [[String: Any]] ?? []) + [["root": 900, "material": "steel"]]

        let m = EditorModel()
        _ = try await adopt(m, doc, link: "b")
        let scene = m.buildScene()
        XCTAssertEqual(m.assemblyInstanceCount, 4, "FK and playback still see only the instances")
        XCTAssertEqual(scene.instances.count, 5, "the mesh is drawn too")
        XCTAssertEqual(scene.instances.last?.index, 4)
    }

    // MARK: defect 5 — attach

    func testAttachLinksTheOpenDocumentAndProtectsIt() async throws {
        let m = EditorModel()
        try await userEdit(m)
        let r = try await call(m, "attach_document", ["link_id": "s"])
        XCTAssertEqual(r["attached"] as? Bool, true)
        XCTAssertNotNil(r["document"] as? [String: Any])
        XCTAssertEqual(m.agentLinkID, "s")

        let status = try await call(m, "link_status", ["link_id": "s"])
        XCTAssertEqual(status["edited"] as? Bool, false)
        let intruder = try await adopt(m, box(20), link: "other")
        XCTAssertEqual(intruder["reason"] as? String, "busy", "an attached document is the user's")
        let own = try await adopt(m, box(30), link: "s")
        XCTAssertEqual(own["adopted"] as? Bool, true)
    }

    func testAttachToAnEmptyWindowSaysSo() async throws {
        let r = try await call(EditorModel(), "attach_document", ["link_id": "s"])
        XCTAssertEqual(r["attached"] as? Bool, false)
    }

    // MARK: defect 6 — relative mesh paths

    func testAdoptedDocumentResolvesRelativeMeshesFromTheSessionFolder() async throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("link-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        try Self.triangleSTL().write(to: dir.appendingPathComponent("tri.stl"))
        let doc: [String: Any] = [
            "version": "0.1",
            "nodes": ["1": ["id": 1, "name": "mesh", "op": ["type": "mesh_import", "path": "tri.stl"]]],
            "materials": [String: Any](), "part_materials": [String: Any](),
            "roots": [["root": 1, "material": "default"]],
        ]

        let without = EditorModel()
        _ = try await adopt(without, doc, link: "a")
        XCTAssertEqual(without.buildScene().triangleCount, 0, "no folder, nothing to resolve from")

        let with = EditorModel()
        _ = try await adopt(with, doc, link: "b", base: dir.path)
        XCTAssertEqual(with.documentDirectory, dir.path)
        XCTAssertEqual(with.buildScene().triangleCount, 1)
    }

    private static func triangleSTL() -> Data {
        var d = Data(count: 80)
        func u32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
        func f(_ v: Float) { u32(v.bitPattern) }
        u32(1)
        for v: Float in [0, 0, 1, 0, 0, 0, 10, 0, 0, 0, 10, 0] { f(v) }
        d.append(contentsOf: [0, 0])
        return d
    }
}
