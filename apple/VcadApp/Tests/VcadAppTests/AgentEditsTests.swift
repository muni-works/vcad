import XCTest
@testable import VcadApp

/// The MCP write surface driven exactly as an agent drives it — through the
/// Rust core the app hosts: every edit is one named undo step, applied to the
/// live model.
@MainActor
final class AgentEditsTests: XCTestCase {
    private func call(_ m: EditorModel, _ name: String, _ args: [String: Any] = [:]) async throws -> String {
        let r = await coreCall(m, name, args)
        let text = resultText(r)
        XCTAssertNil(r["isError"], "\(name) failed: \(text)")
        return text
    }

    private func created(_ text: String) throws -> String {
        let r = try XCTUnwrap(text.range(of: "created part "), text)
        return String(text[r.upperBound...].prefix { $0 != ";" && $0 != "." && $0 != " " })
    }

    /// A refusal is an MCP `isError` result with a reason.
    private func assertRefused(_ m: EditorModel, _ name: String, _ args: [String: Any]) async {
        let r = await coreCall(m, name, args)
        XCTAssertEqual(r["isError"] as? Bool, true, "\(name) should refuse: \(r)")
        XCTAssertFalse(resultText(r).isEmpty)
    }

    private func roots(_ m: EditorModel) -> Int { (m.documentJSON?["roots"] as? [Any])?.count ?? 0 }

    func testAssemblyInstancesAppearInTheTree() throws {
        let json: [String: Any] = [
            "version": "0.1",
            "nodes": [
                "1": ["id": 1, "name": "Base", "op": ["type": "Cube", "size": ["x": 1, "y": 1, "z": 1]]],
                "2": ["id": 2, "name": "Link", "op": ["type": "Cube", "size": ["x": 2, "y": 1, "z": 1]]],
            ],
            "roots": [["root": 1, "material": "default"]],
            "partDefs": ["d1": ["id": "d1", "name": "Link", "root": 2]],
            "instances": [["id": "i1", "partDefId": "d1", "name": "Upper"],
                          ["id": "i2", "partDefId": "d1", "name": "Lower"]],
        ]
        let data = try JSONSerialization.data(withJSONObject: json)
        let graph = try XCTUnwrap(DocumentGraph.parse(data))
        let rows = graph.featureRoots()
        XCTAssertEqual(rows.map(\.name), ["Base", "Upper", "Lower"])
        XCTAssertEqual(rows[1].id, "instance:i1")
        XCTAssertEqual(rows[1].nodeId, 2)
    }

    func testToolsAreListedWithSchemas() async {
        let tools = await coreTools(EditorModel())
        for n in ["create", "update", "delete", "set_material", "difference", "set_parameters",
                  "create_cad_loon", "get_document", "inspect_cad", "describe_scene", "snapshot",
                  "verify_receipts", "fab_print", "optimize_parameters", "agent_turn", "pick_in_viewport"] {
            XCTAssertNotNil(tools[n], "missing tool \(n)")
        }
        for (n, t) in tools { XCTAssertNotNil(t["inputSchema"] as? [String: Any], n) }
    }

    /// The gate, headless: an empty window → a filleted bracket with a hole,
    /// one undo step per call, walked back one at a time.
    func testFilletedBracketWithHoleFromEmptyThenUndoStepByStep() async throws {
        let m = EditorModel()
        XCTAssertNil(m.documentJSON)

        let plate = try created(try await call(m, "create", [
            "type": "cube", "name": "Bracket", "params": ["size": ["x": 60, "y": 30, "z": 8]]]))
        let hole = try created(try await call(m, "create", [
            "type": "cylinder", "name": "Hole", "params": ["radius": 4, "height": 20]]))
        _ = try await call(m, "translate", ["child": hole, "offset": ["x": 45, "y": 15, "z": -5]])
        let cut = try created(try await call(m, "difference", ["left": plate, "right": hole, "name": "Drilled"]))
        _ = try await call(m, "create", ["type": "fillet", "name": "Edges", "parent_part_id": cut,
                                         "params": ["radius": 2]])
        _ = try await call(m, "set_material", ["part_id": cut, "material": "aluminum"])

        XCTAssertEqual(m.undoStack.map(\.name), [
            "Claude: Create Bracket", "Claude: Create Hole", "Claude: Translate",
            "Claude: Difference Drilled", "Claude: Create Edges", "Claude: Set Material",
        ])
        XCTAssertEqual(roots(m), 1, "difference + fillet leave one part")

        // Volume: plate minus the through hole, minus a little for the fillets.
        let measured = try await call(m, "inspect_cad")
        XCTAssertTrue(measured.lowercased().contains("volume"), measured)

        let expected = [1, 1, 2, 2, 1, 0]   // roots after undoing each step, newest first
        for want in expected {
            m.undo()
            XCTAssertEqual(roots(m), want, "after undo, stack \(m.undoStack.map(\.name))")
        }
        XCTAssertTrue(m.undoStack.isEmpty)
        XCTAssertEqual(m.redoStack.count, 6)
    }

    func testRefusedEditLeavesDocumentAndUndoUntouched() async throws {
        let m = EditorModel()
        _ = try await call(m, "create", ["type": "cube", "params": ["size": ["x": 10, "y": 10, "z": 10]]])
        let before = m.undoStack.count
        await assertRefused(m, "difference", ["left": "nope", "right": "nada"])
        XCTAssertEqual(m.undoStack.count, before)
        XCTAssertEqual(roots(m), 1)
    }

    func testLoonProgramAndParameters() async throws {
        let m = EditorModel()
        _ = try await call(m, "create_cad_loon", [
            "name": "Plate",
            "source": "[defparam thick 5.0 :min 1 :max 20]\n[pub let plate [cube 40.0 20.0 thick]]",
        ])
        XCTAssertEqual(m.undoStack.last?.name, "Claude: Plate")
        XCTAssertEqual(roots(m), 1)
        let params = try await call(m, "list_parameters")
        XCTAssertTrue(params.contains("thick"), params)

        let set = try await call(m, "set_parameters", ["values": ["thick": 50]])
        XCTAssertTrue(set.contains("thick=20"), "clamped to max: \(set)")
        XCTAssertEqual(m.undoStack.last?.name, "Claude: Set thick")
        XCTAssertEqual(m.docParameters.first { $0.name == "thick" }?.value, 20)
        m.undo()
        XCTAssertEqual(m.docParameters.first { $0.name == "thick" }?.value, 5)

        await assertRefused(m, "create_cad_loon", ["source": "[cube 1 2"])
        XCTAssertEqual(m.undoStack.count, 1, "a loon error adds no undo step")
    }

    /// An agent's undo over the bridge targets the document. While the user
    /// has a sketch open, ⌘Z is scoped to the sketch — so the agent's undo
    /// refuses plainly instead of eating the user's sketch strokes.
    func testAgentUndoRefusesWhileSketching() async throws {
        let m = EditorModel()
        _ = try await call(m, "create", ["type": "cube", "name": "Block",
                                         "params": ["size": ["x": 40, "y": 40, "z": 10]]])
        let steps = m.undoStack.map(\.name)
        m.enterSketch(); m.setSketchTool(.rectangle)
        m.sketchTap([50, 0]); m.sketchTap([60, 10])
        let sketch = try XCTUnwrap(m.sketch, m.engineError ?? "sketch did not open")
        let lines = sketch.entities.filter { $0.kind == .line }.count
        XCTAssertEqual(lines, 4)

        let refused = await coreCall(m, "undo")
        XCTAssertEqual(refused["isError"] as? Bool, true, "agent undo should refuse while a sketch is open")
        XCTAssertTrue(resultText(refused).contains("sketch"), resultText(refused))
        XCTAssertEqual(sketch.entities.filter { $0.kind == .line }.count, lines, "sketch untouched")
        XCTAssertEqual(m.undoStack.map(\.name), steps, "document history untouched")
        XCTAssertEqual(roots(m), 1)

        // Out of the sketch, the agent's undo reaches the document.
        m.exitSketch()
        _ = try await call(m, "undo")
        XCTAssertEqual(roots(m), 0)
    }

    func testUndoNames() {
        XCTAssertEqual(AgentEdits.undoName("create", ["type": "cylinder"]), "Claude: Create Cylinder")
        XCTAssertEqual(AgentEdits.undoName("linear_pattern", [:]), "Claude: Linear Pattern")
        XCTAssertEqual(AgentEdits.undoName("union", ["name": "Body"]), "Claude: Union Body")
    }
}

final class ConnectClaudeTests: XCTestCase {
    func testInstallMergesWithoutDroppingKeysAndIsIdempotent() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("cc-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: dir) }
        let targets = ConnectClaude.targets(home: dir)
        XCTAssertEqual(targets.map(\.label), ["Claude Code", "Claude Desktop"])

        // Existing config with other servers and unrelated keys.
        let code = targets[0].config
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let existing: [String: Any] = ["numStartups": 7, "mcpServers": ["other": ["command": "x"]]]
        try JSONSerialization.data(withJSONObject: existing).write(to: code)

        let entry = ConnectClaude.entry(node: "/opt/homebrew/bin/node", shim: "/tmp/vcad-app-mcp.mjs")
        XCTAssertTrue(try ConnectClaude.install(entry: entry, into: code))
        XCTAssertFalse(try ConnectClaude.install(entry: entry, into: code), "second run is a no-op")
        let root = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: code)) as? [String: Any])
        XCTAssertEqual(root["numStartups"] as? Int, 7)
        let servers = try XCTUnwrap(root["mcpServers"] as? [String: Any])
        XCTAssertNotNil(servers["other"])
        XCTAssertEqual((servers["vcad-app"] as? [String: Any])?["args"] as? [String], ["/tmp/vcad-app-mcp.mjs"])
        XCTAssertTrue(FileManager.default.fileExists(atPath: code.path + ".vcad-backup"))

        // Desktop config that doesn't exist yet (nor its folder).
        XCTAssertTrue(try ConnectClaude.install(entry: entry, into: targets[1].config))
        XCTAssertTrue(FileManager.default.fileExists(atPath: targets[1].config.path))
    }

    func testShimIsFound() {
        XCTAssertNotNil(ConnectClaude.shimPath())
    }
}
