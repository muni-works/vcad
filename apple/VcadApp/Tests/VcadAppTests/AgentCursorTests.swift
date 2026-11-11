import XCTest
@testable import VcadApp

/// MCP-native M1: the live document as resources, agent turns as single
/// undo steps, and the agent cursor landing on what a tool touched.
@MainActor
final class AgentCursorTests: XCTestCase {
    override func setUp() async throws {
        AgentTurn.shared.end()
        AgentTurn.shared.now = Date.init
        AgentResources.shared.sent.removeAll()
        AgentCursor.shared.hide()
    }

    @discardableResult
    private func call(_ m: EditorModel, _ name: String, _ args: [String: Any] = [:]) async throws -> String {
        let r = await coreCall(m, name, args)
        let text = ((r["content"] as? [[String: Any]])?.first { $0["type"] as? String == "text" }?["text"] as? String) ?? ""
        XCTAssertNil(r["isError"], "\(name) failed: \(text)")
        return text
    }

    private func created(_ text: String) throws -> String {
        let r = try XCTUnwrap(text.range(of: "created part "), text)
        return String(text[r.upperBound...].prefix { $0 != ";" && $0 != "." && $0 != " " })
    }

    private func cube(_ m: EditorModel, _ name: String = "Box") async throws -> String {
        try created(try await call(m, "create", ["type": "cube", "name": name,
                                                 "params": ["size": ["x": 40, "y": 20, "z": 10]]]))
    }

    // MARK: turns

    func testATurnIsOneNamedUndoStep() async throws {
        let m = EditorModel()
        let base = try await cube(m, "Base")
        XCTAssertEqual(m.undoStack.map(\.name), ["Claude: Create Base"])

        try await call(m, "agent_turn", ["label": "thickened walls"])
        let post = try await cube(m, "Post")
        try await call(m, "translate", ["child": post, "offset": ["x": 5, "y": 5, "z": 10]])
        try await call(m, "union", ["left": base, "right": post, "name": "Body"])
        try await call(m, "agent_turn", ["end": true])

        XCTAssertEqual(m.undoStack.map(\.name), ["Claude: Create Base", "Agent: thickened walls"])
        m.undo()
        XCTAssertEqual((m.documentJSON?["roots"] as? [Any])?.count, 1, "one ⌘Z rewinds the whole turn")
        XCTAssertEqual(m.undoStack.map(\.name), ["Claude: Create Base"])

        // After the turn closes, edits are their own steps again.
        m.redo()
        _ = try await cube(m, "Loose")
        XCTAssertEqual(m.undoStack.last?.name, "Claude: Create Loose")
    }

    func testAUserEditSplitsTheTurn() async throws {
        let m = EditorModel()
        try await call(m, "agent_turn", ["label": "drilled holes"])
        _ = try await cube(m, "A")
        // The user fillets by hand mid-turn; the agent's next edit must not
        // fold into an entry that would undo the user's work with it.
        m.selectParts([0])
        XCTAssertTrue(m.applyBlend(fillet: true, size: 1))
        _ = try await cube(m, "B")
        XCTAssertEqual(m.undoStack.map(\.name), ["Agent: drilled holes", "Fillet", "Agent: drilled holes"])
    }

    func testTurnClosesAfterIdle() async throws {
        let m = EditorModel()
        var clock = Date()
        AgentTurn.shared.now = { clock }
        try await call(m, "agent_turn", ["label": "first"])
        _ = try await cube(m, "A")
        clock += AgentTurn.idleTimeout + 1
        _ = try await cube(m, "B")
        XCTAssertEqual(m.undoStack.map(\.name), ["Agent: first", "Claude: Create B"])
    }

    // MARK: resources

    func testResourcesReadTheLiveDocument() async throws {
        let m = EditorModel()
        AgentResources.shared.store = AppStore(model: m)
        let list = AgentResources.shared.list()["resources"] as? [[String: Any]] ?? []
        XCTAssertEqual(Set(list.compactMap { $0["uri"] as? String }),
                       ["vcad://doc/current", "vcad://doc/selection", "vcad://doc/view", "ui://vcad/viewer"])

        _ = try await cube(m)
        let doc = try XCTUnwrap(((AgentResources.shared.read("vcad://doc/current")["contents"] as? [[String: Any]])?
            .first?["text"] as? String))
        XCTAssertTrue(doc.contains("\"Box\""), doc)

        let viewer = try XCTUnwrap(((AgentResources.shared.read(AgentResources.viewerURI)["contents"] as? [[String: Any]])?
            .first?["text"] as? String))
        XCTAssertTrue(viewer.contains("viewer_frame") && viewer.contains("open_in_app"))
    }

    func testSubscribersHearAgentEditsAndUserSelection() async throws {
        let m = EditorModel()
        AgentResources.shared.store = AppStore(model: m)
        try AgentResources.shared.subscribe("vcad://doc/current")
        try AgentResources.shared.subscribe("vcad://doc/selection")
        defer {
            AgentResources.shared.unsubscribe("vcad://doc/current")
            AgentResources.shared.unsubscribe("vcad://doc/selection")
        }
        AgentResources.shared.poll()
        XCTAssertTrue(AgentResources.shared.sent.isEmpty, "nothing changed yet")

        _ = try await cube(m)
        AgentResources.shared.poll()
        XCTAssertEqual(updatedURIs(), ["vcad://doc/current"])

        AgentResources.shared.sent.removeAll()
        m.selectParts([0])
        AgentResources.shared.poll()
        XCTAssertEqual(updatedURIs(), ["vcad://doc/selection"])
        let sel = try XCTUnwrap(AppStore(model: m).selection()["parts"] as? [[String: Any]])
        XCTAssertEqual(sel.first?["index"] as? Int, 0)
    }

    private func updatedURIs() -> [String] {
        AgentResources.shared.sent.compactMap { ($0["params"] as? [String: Any])?["uri"] as? String }
    }

    // MARK: the demo: select an edge, agent fillets it, the cursor lands there

    func testFilletSelectedEdgeMovesTheCursorToIt() async throws {
        let m = EditorModel()
        _ = try await cube(m)
        m.selectParts([0])
        let topo = try XCTUnwrap(m.partTopology(0))
        let edge = try XCTUnwrap(topo.edges.first)
        m.picking.filter = .edge
        m.picking.setSelection([.edge(edge.id, part: 0)])

        let sel = AppStore(model: m).selection()["topology"] as? [[String: Any]]
        XCTAssertEqual(sel?.first?["kind"] as? String, "edge", "the agent sees the user's edge")

        try await call(m, "agent_turn", ["label": "rounded the edge"])
        let text = try await call(m, "fillet_edges", ["radius": 2])
        XCTAssertTrue(text.contains("1 selected edge"), text)
        XCTAssertEqual(m.undoStack.last?.name, "Agent: rounded the edge")

        let cursor = AgentCursor.shared
        XCTAssertEqual(cursor.target, .point(edge.midpoint))
        XCTAssertTrue(cursor.label.hasSuffix("· rounded the edge"), cursor.label)
    }

    func testCursorFallsBackToTheTouchedPart() async throws {
        let m = EditorModel()
        _ = try await cube(m)
        XCTAssertEqual(AgentCursor.shared.target, .part(0))
        XCTAssertEqual(AgentCursor.shared.label, "Agent · Create Box")
    }

    func testCaptionsAndClientNames() {
        XCTAssertEqual(AgentCursor.caption("Agent: thickened walls", agent: "Claude"), "Claude · thickened walls")
        XCTAssertEqual(AgentCursor.caption("Claude: Fillet Edges", agent: "Cursor"), "Cursor · Fillet Edges")
        XCTAssertEqual(AgentBridge.displayName("claude-code"), "Claude")
        XCTAssertEqual(AgentBridge.displayName("cursor-vscode"), "Cursor")
    }

    func testMutatingToolsAdvertiseTheViewer() async {
        let tools = await coreTools(EditorModel())
        XCTAssertNotNil(tools["create"]?["_meta"])
        XCTAssertNotNil(tools["fillet_edges"]?["_meta"])
        XCTAssertNil(tools["get_document"]?["_meta"])
    }
}
