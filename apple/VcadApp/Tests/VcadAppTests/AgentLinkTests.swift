import XCTest
import simd
@testable import VcadApp

/// The live link to `@vcad/mcp`, driven exactly as an agent drives it (through
/// the Rust MCP core the app hosts).
@MainActor
final class AgentLinkTests: XCTestCase {
    private static let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()

    override func setUp() async throws {
        GlassUI.shared.space = .design
    }

    private func call(_ m: EditorModel, _ name: String, _ args: [String: Any] = [:]) async throws -> [String: Any] {
        let r = await coreCall(m, name, args)
        let text = resultText(r)
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

    func testAToolMintedSessionNeverTakesALiveWindow() async throws {
        let m = EditorModel()
        _ = try await adopt(m, box(10), link: "a")
        let throwaway = try await adopt(m, box(20), link: "b")
        XCTAssertEqual(throwaway["reason"] as? String, "linked", "\(throwaway)")
        XCTAssertEqual(m.agentLinkID, "a")
        m.agentLinkActiveAt = Date(timeIntervalSinceNow: -3600)
        let later = try await adopt(m, box(20), link: "b")
        XCTAssertEqual(later["reason"] as? String, "linked", "idleness alone isn't intent")
        XCTAssertEqual(m.agentLinkID, "a")
    }

    func testAnOpenedSessionWaitsForTheOldOneToGoIdle() async throws {
        let m = EditorModel()
        _ = try await adopt(m, box(10), link: "a")
        var args: [String: Any] = ["document": box(20), "link_id": "b", "tool": "create_cad_loon", "intent": "opened"]
        let early = try await call(m, "adopt_document", args)
        XCTAssertEqual(early["reason"] as? String, "linked_active")
        m.agentLinkActiveAt = Date(timeIntervalSinceNow: -(AgentEdits.takeoverIdleSeconds + 1))
        args["tool"] = "update"
        let idle = try await call(m, "adopt_document", args)
        XCTAssertEqual(idle["adopted"] as? Bool, true, "\(idle)")
        XCTAssertEqual(idle["displaced"] as? String, "a", "the loser must be named so it can be told")
        XCTAssertEqual(m.agentLinkID, "b")
        XCTAssertEqual(m.undoStack.count, 2, "a takeover is one undo step, so it can be walked back")
    }

    func testAnExplicitCallTakesAnAgentsWindowButNeverTheUsers() async throws {
        let m = EditorModel()
        _ = try await adopt(m, box(10), link: "a")
        let force: [String: Any] = ["document": box(20), "link_id": "b", "tool": "show_in_app", "intent": "explicit"]
        let taken = try await call(m, "adopt_document", force)
        XCTAssertEqual(taken["displaced"] as? String, "a")
        try await userEdit(m)
        var other = force; other["link_id"] = "c"
        let refused = try await call(m, "adopt_document", other)
        XCTAssertEqual(refused["reason"] as? String, "busy")
    }

    func testAPoseAndAStatusCheckKeepASessionActive() async throws {
        let m = EditorModel()
        _ = try await adopt(m, try arm(), link: "a")
        m.agentLinkActiveAt = nil
        let name = try XCTUnwrap(m.drivenJoints.first).name ?? ""
        _ = try await call(m, "set_pose", ["joints": [name: 5], "link_id": "a"])
        XCTAssertNotNil(m.agentLinkActiveAt)
        m.agentLinkActiveAt = nil
        _ = try await call(m, "link_status", ["link_id": "a"])
        XCTAssertNotNil(m.agentLinkActiveAt)
    }

    func testTakeoverRefusalMatrix() {
        let idle = AgentEdits.takeoverIdleSeconds
        typealias T = AgentEdits
        XCTAssertNil(T.takeoverRefusal(scratch: true, agentsOnly: false, intent: nil, idleSeconds: 0))
        XCTAssertEqual(T.takeoverRefusal(scratch: false, agentsOnly: false, intent: "explicit", idleSeconds: 999), "busy")
        XCTAssertEqual(T.takeoverRefusal(scratch: false, agentsOnly: true, intent: nil, idleSeconds: 999), "linked")
        XCTAssertEqual(T.takeoverRefusal(scratch: false, agentsOnly: true, intent: "opened", idleSeconds: idle - 1), "linked_active")
        XCTAssertNil(T.takeoverRefusal(scratch: false, agentsOnly: true, intent: "opened", idleSeconds: idle))
        XCTAssertNil(T.takeoverRefusal(scratch: false, agentsOnly: true, intent: "explicit", idleSeconds: 0))
    }

    // MARK: set_pose input

    private func expectPoseFailure(_ m: EditorModel, _ args: [String: Any], contains: String,
                                   line: UInt = #line) async throws {
        let before = m.jointPose
        do {
            _ = try await AgentTools.call("set_pose", args, model: m)
            XCTFail("should refuse \(args)", line: line)
        } catch let f as AgentTools.Failure {
            XCTAssertTrue(f.message.contains(contains), f.message, line: line)
        }
        XCTAssertEqual(m.jointPose, before, "a refused call changes nothing", line: line)
    }

    func testSetPoseRejectsBadShapesAndChangesNothing() async throws {
        let m = EditorModel()
        _ = try await adopt(m, try arm(), link: "a")
        let name = try XCTUnwrap(m.drivenJoints.first).name ?? ""
        _ = try await call(m, "set_pose", ["joints": [name: 12]])
        try await expectPoseFailure(m, ["joint": name, "value": 30], contains: "unknown argument joint, value")
        try await expectPoseFailure(m, [:], contains: "nothing to apply")
        try await expectPoseFailure(m, ["joints": [String: Any]()], contains: "nothing to apply")
        try await expectPoseFailure(m, ["joints": 3], contains: "must be an object")
        try await expectPoseFailure(m, ["joints": [name: "up"]], contains: "number of degrees")
        try await expectPoseFailure(m, ["joints": [name: 20, "nope": 1]], contains: "unknown joints nope")
        try await expectPoseFailure(m, ["base": [1, 2]], contains: "7 numbers")
        try await expectPoseFailure(m, ["joints": [name: 20], "link_id": "other"], contains: "different session")
        XCTAssertEqual(m.jointPose[name], 12)
    }

    func testLinkStatusIsPerSessionAndSaysWhy() async throws {
        let m = EditorModel()
        var r = try await call(m, "link_status", ["link_id": "a"])
        XCTAssertEqual(r["linked"] as? Bool, false)
        XCTAssertEqual(r["window_linked"] as? Bool, false)
        XCTAssertEqual(r["reason"] as? String, "no_link")

        _ = try await adopt(m, box(10), link: "a")
        r = try await call(m, "link_status", ["link_id": "a"])
        XCTAssertEqual(r["linked"] as? Bool, true)

        r = try await call(m, "link_status", ["link_id": "b"])
        XCTAssertEqual(r["linked"] as? Bool, false, "@vcad/mcp reads false as this session losing the window")
        XCTAssertEqual(r["window_linked"] as? Bool, true)
        XCTAssertEqual(r["reason"] as? String, "other_session")

        r = try await call(m, "link_status")
        XCTAssertEqual(r["linked"] as? Bool, false)
        XCTAssertEqual(r["window_linked"] as? Bool, true)
        XCTAssertEqual(r["reason"] as? String, "no_link_id")
    }

    func testAgentBuildFramesTheContentUntilTheUserMovesTheCamera() async throws {
        let m = EditorModel()
        _ = try await adopt(m, box(10), link: "a")
        XCTAssertTrue(m.agentFramePending)
        m.sizeMM = SIMD3(200, 40, 40)
        let first = try XCTUnwrap(m.takeAgentFrameDistance(aspect: 1.6), "first geometry frames")
        XCTAssertLessThan(first, 1.5, "the default distance leaves the model small")

        _ = try await adopt(m, box(11), link: "a")
        m.sizeMM = SIMD3(205, 40, 40)
        XCTAssertNil(m.takeAgentFrameDistance(aspect: 1.6), "a small change leaves the camera alone")

        _ = try await adopt(m, box(12), link: "a")
        m.sizeMM = SIMD3(200, 40, 300)
        XCTAssertNotNil(m.takeAgentFrameDistance(aspect: 1.6), "a large change reframes")

        m.beginOrbit()
        _ = try await adopt(m, box(13), link: "a")
        m.sizeMM = SIMD3(900, 900, 40)
        XCTAssertNil(m.takeAgentFrameDistance(aspect: 1.6), "the camera is the user's now")
    }

    func testFittedDistanceKeepsEveryCornerInView() {
        for size in [SIMD3<Float>(100, 100, 100), SIMD3(300, 10, 10), SIMD3(10, 10, 300)] {
            for aspect: Float in [0.7, 1.0, 2.2] {
                let (az, el) = (Float.pi / 5, Float.pi / 7)
                let d = EditorModel.fittedDistance(size: size, azimuth: az, elevation: el, aspect: aspect)
                let world = SIMD3(size.x, size.z, size.y) * (0.6 / size.max()) / 2
                let toCamera = SIMD3(cos(el) * sin(az), sin(el), cos(el) * cos(az))
                let right = normalize(cross(-toCamera, SIMD3<Float>(0, 1, 0)))
                let up = cross(right, -toCamera)
                let tanHalf = tan(Float.pi / 6)
                var widest: Float = 0
                for sx: Float in [-1, 1] { for sy: Float in [-1, 1] { for sz: Float in [-1, 1] {
                    let c = world * SIMD3(sx, sy, sz)
                    let depth = d - dot(c, toCamera)
                    XCTAssertGreaterThan(depth, 0)
                    widest = max(widest, abs(dot(c, up)) / (depth * tanHalf),
                                 abs(dot(c, right)) / (depth * tanHalf * aspect))
                }}}
                // At the dolly's near limit the box may sit inside the margin.
                XCTAssertLessThanOrEqual(widest, 0.81, "\(size) @ \(aspect) stays in the margin")
                if d > 0.45 { XCTAssertEqual(widest, 0.8, accuracy: 0.01, "\(size) @ \(aspect) fills the margin") }
            }
        }
    }

    func testAMirroredStreamPosesWhatMatches() async throws {
        let m = EditorModel()
        _ = try await adopt(m, try arm(), link: "a")
        let name = try XCTUnwrap(m.drivenJoints.first).name ?? ""
        let r = try await call(m, "set_pose", ["joints": [name: 15, "welded": 0], "partial": true, "link_id": "a"])
        XCTAssertEqual(m.jointPose[name], 15)
        XCTAssertTrue((r["text"] as? String ?? "").contains("welded"), "\(r)")
    }

    func testAFixedJointInAPoseIsSkippedNotUnknown() async throws {
        var doc = try arm()
        var joints = try XCTUnwrap(doc["joints"] as? [[String: Any]])
        let driven = try XCTUnwrap(joints.first?["name"] as? String)
        joints[1]["kind"] = ["type": "Fixed"]
        joints[1]["name"] = "weld"
        doc["joints"] = joints
        let m = EditorModel()
        _ = try await adopt(m, doc, link: "a")
        _ = try await call(m, "set_pose", ["joints": [driven: 9, "weld": 3]])
        XCTAssertEqual(m.jointPose[driven], 9)
        XCTAssertNil(m.jointPose["weld"])
    }

    func testAssemblyRootsSurviveAdoptionAndPull() async throws {
        var doc = try arm()
        // One scene root; the other part definitions exist only through instances.
        doc["roots"] = Array(try XCTUnwrap(doc["roots"] as? [Any]).prefix(1))
        let before = (doc["roots"] as? [Any])?.count
        let m = EditorModel()
        _ = try await adopt(m, doc, link: "a")
        XCTAssertEqual((m.documentJSON?["roots"] as? [Any])?.count, before, "part definitions must not become roots")
        try await userEdit(m)
        let pulled = try await call(m, "link_status", ["link_id": "a", "pull": true])
        let roots = (pulled["document"] as? [String: Any])?["roots"] as? [Any]
        XCTAssertEqual(roots?.count, (before ?? 0) + 1, "only the user's part is new")
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
