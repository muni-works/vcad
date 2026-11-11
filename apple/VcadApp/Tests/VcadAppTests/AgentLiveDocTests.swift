import XCTest
@testable import VcadApp

/// MCP-native Phase B: the agent works in the user's window — selection
/// references, pinned document scope, ghost review of destructive edits,
/// client names on the cursor, "fillet this face".
@MainActor
final class AgentLiveDocTests: XCTestCase {
    override func setUp() async throws {
        AgentTurn.shared.end()
        AgentCursor.shared.hide()
        AgentCursor.shared.agentName = "Agent"
        AgentGhost.shared.autoDecision = true
        AgentGhost.shared.asked.removeAll()
        AgentGhost.shared.agentCreated.removeAll()
    }

    override func tearDown() async throws {
        AgentGhost.shared.autoDecision = nil
        AgentTurn.shared.end()
    }

    private func call(_ m: EditorModel, _ name: String, _ args: [String: Any] = [:]) async -> (String, Bool) {
        let r = await coreCall(m, name, args)
        return (resultText(r), r["isError"] as? Bool ?? false)
    }

    private func cube(_ m: EditorModel, _ name: String) async throws -> String {
        let (text, err) = await call(m, "create", ["type": "cube", "name": name,
                                                   "params": ["size": ["x": 40, "y": 20, "z": 10]]])
        XCTAssertFalse(err, text)
        let r = try XCTUnwrap(text.range(of: "created part "), text)
        return String(text[r.upperBound...].prefix { $0 != ";" && $0 != "." && $0 != " " })
    }

    // MARK: selection references

    func testSelectionResolvesToTheUsersPart() async throws {
        let m = EditorModel()
        _ = try await cube(m, "A")
        _ = try await cube(m, "B")
        m.selectParts([1])
        let (text, err) = await call(m, "set_material", ["part_id": "selection", "material": "aluminum"])
        XCTAssertFalse(err, text)
        let resolved = try AgentSelection.resolve(["left": "selection", "part_ids": ["selection"]], m)
        XCTAssertEqual(resolved["left"] as? String, m.partId(forPart: 1))
        XCTAssertEqual(resolved["part_ids"] as? [String], [m.partId(forPart: 1)].compactMap { $0 })
    }

    func testEmptySelectionIsAClearError() async throws {
        let m = EditorModel()
        _ = try await cube(m, "A")
        m.selectParts([])
        XCTAssertThrowsError(try AgentSelection.resolve(["part_id": "selection"], m)) { e in
            XCTAssertTrue((e as? AgentTools.Failure)?.message.contains("nothing selected") ?? false)
        }
    }

    func testViewAndSelectionResourcesCarryContext() async throws {
        let m = EditorModel()
        _ = try await cube(m, "A")
        let store = AppStore(model: m)
        let view = store.view()
        XCTAssertEqual((view["visible_parts"] as? [[String: Any]])?.count, 1)
        XCTAssertEqual(view["sketching"] as? Bool, false)
        XCTAssertEqual(view["document_id"] as? String, AgentCore.documentId(m))

        m.selectParts([0])
        let topo = try XCTUnwrap(m.partTopology(0))
        let face = try XCTUnwrap(topo.faces.first)
        m.picking.filter = .face
        m.picking.setSelection([TopoRef(part: 0, kind: .face, id: face.id)])
        let sel = store.selection()
        let first = try XCTUnwrap((sel["topology"] as? [[String: Any]])?.first)
        XCTAssertEqual(first["kind"] as? String, "face")
        XCTAssertNotNil(first["center"], "\"this face\" has a position")
        XCTAssertEqual(sel["ref"] as? String, "selection")
    }

    // MARK: fillet this face

    func testFilletASelectedFaceRoundsItsBoundary() async throws {
        let m = EditorModel()
        let (made, madeErr) = await call(m, "create", ["type": "cylinder", "name": "Post",
                                                       "params": ["radius": 10, "height": 20]])
        XCTAssertFalse(madeErr, made)
        m.selectParts([0])
        let topo = try XCTUnwrap(m.partTopology(0))
        let top = try XCTUnwrap(topo.faces.max { $0.center[2] < $1.center[2] })
        m.picking.filter = .face
        m.picking.setSelection([TopoRef(part: 0, kind: .face, id: top.id)])
        let (text, err) = await call(m, "fillet_edges", ["radius": 2])
        XCTAssertFalse(err, text)
        XCTAssertTrue(text.contains("edges around 1 selected face") || text.contains("1 edges around"), text)
    }

    // MARK: scope

    func testStaleDocumentIdIsRefusedAfterTheUserSwitches() async throws {
        let m = EditorModel()
        AgentCore.shared.attach(m)
        let pinned = AgentCore.documentId(m)
        _ = try await cube(m, "A")
        let (ok, okErr) = await call(m, "describe_scene", ["document_id": pinned])
        XCTAssertFalse(okErr, ok)

        m.newDocument()                              // the user opens something else
        XCTAssertNotEqual(AgentCore.documentId(m), pinned)
        let (text, err) = await call(m, "create", ["document_id": pinned, "type": "cube", "name": "Intruder",
                                                   "params": ["size": ["x": 1, "y": 1, "z": 1]]])
        XCTAssertTrue(err, text)
        XCTAssertTrue(text.contains("SCOPE"), text)
        XCTAssertNil(m.documentJSON.flatMap { ($0["roots"] as? [Any])?.isEmpty == false ? $0 : nil },
                     "nothing landed in the new document")
    }

    func testATurnDoesNotFollowTheUserIntoAnotherDocument() async throws {
        let m = EditorModel()
        AgentCore.shared.attach(m)
        _ = await call(m, "agent_turn", ["label": "add posts"])
        _ = try await cube(m, "A")
        m.newDocument()
        let (text, err) = await call(m, "create", ["type": "cube", "name": "B",
                                                   "params": ["size": ["x": 1, "y": 1, "z": 1]]])
        XCTAssertTrue(err, text)
        XCTAssertTrue(text.contains("SCOPE"), text)
    }

    // MARK: ghost review

    func testDeletingTheUsersPartWaitsForReviewAndRejectUndoesIt() async throws {
        let m = EditorModel()
        let id = try await cube(m, "Keeper")
        AgentGhost.shared.agentCreated.removeAll()   // as if the user had made it
        AgentGhost.shared.autoDecision = false
        let depth = m.undoStack.count
        let (text, err) = await call(m, "delete", ["part_id": id])
        XCTAssertTrue(err, text)
        XCTAssertTrue(text.contains("rejected"), text)
        XCTAssertEqual(AgentGhost.shared.asked.count, 1)
        XCTAssertEqual((m.documentJSON?["roots"] as? [Any])?.count, 1, "reject rolled the delete back")
        XCTAssertEqual(m.undoStack.count, depth)
    }

    func testApproveKeepsTheDelete() async throws {
        let m = EditorModel()
        let id = try await cube(m, "Gone")
        AgentGhost.shared.agentCreated.removeAll()
        AgentGhost.shared.autoDecision = true
        let (text, err) = await call(m, "delete", ["part_id": id])
        XCTAssertFalse(err, text)
        XCTAssertEqual((m.documentJSON?["roots"] as? [Any])?.count ?? 0, 0)
        XCTAssertEqual(AgentGhost.shared.asked.first?.summary.hasPrefix("removes part"), true)
    }

    func testBooleansOnTheAgentsOwnPartsNeverAsk() async throws {
        let m = EditorModel()
        AgentGhost.shared.autoDecision = false   // would reject if asked
        let a = try await cube(m, "Plate")
        let b = try await cube(m, "Hole")
        let (text, err) = await call(m, "difference", ["left": a, "right": b, "name": "Plate"])
        XCTAssertFalse(err, text)
        XCTAssertEqual(AgentGhost.shared.asked.count, 0)
    }

    func testReasonRules() {
        let g = AgentGhost()
        XCTAssertNil(g.reason(before: ["a", "b"], after: ["a", "b", "c"]))
        XCTAssertNotNil(g.reason(before: ["a", "b"], after: ["a"]))
        g.agentCreated = ["b"]
        XCTAssertNil(g.reason(before: ["a", "b"], after: ["a"]))
        g.agentCreated = ["p", "q", "r"]
        XCTAssertEqual(g.reason(before: ["p", "q", "r"], after: ["p", "x", "y"], wholeDocument: true), "rewrites 2 of 3 parts")
    }

    // MARK: who's there

    func testClientNameLabelsTheCursor() async {
        let m = EditorModel()
        AgentCore.shared.attach(m)
        _ = await AgentCore.shared.rpc("initialize", ["protocolVersion": "2025-06-18",
                                                      "clientInfo": ["name": "claude-code", "version": "1"],
                                                      "capabilities": [String: Any]()])
        XCTAssertEqual(AgentCursor.shared.agentName, "Claude")
    }
}
