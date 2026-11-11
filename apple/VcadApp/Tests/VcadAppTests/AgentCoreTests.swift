import XCTest
@testable import VcadApp

/// Phase A: vcad.app hosts the Rust MCP core. The real app lists every tool
/// in docs/launch/mcp-tool-inventory.md that is native or a host hook, each is
/// callable through the core, and every host tool the core lists has a Swift
/// implementation behind it.
@MainActor
final class AgentCoreTests: XCTestCase {
    private static let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()

    /// `(tool, after Phase A)` rows of the inventory table.
    private func inventory() throws -> [(String, String)] {
        let md = try String(contentsOf: Self.repoRoot.appendingPathComponent("docs/launch/mcp-tool-inventory.md"),
                            encoding: .utf8)
        return md.split(separator: "\n").filter { $0.hasPrefix("| `") }.map { line in
            let cols = line.split(separator: "|", omittingEmptySubsequences: false).map {
                $0.trimmingCharacters(in: .whitespaces)
            }
            return (cols[1].trimmingCharacters(in: CharacterSet(charactersIn: "`")), cols[5])
        }
    }

    /// Host tools that need an on-screen window or minutes of work; their
    /// dispatch is still checked by name below.
    private static let needsWindow: Set<String> = [
        "snapshot", "app_screenshot", "photoreal", "viewer_frame", "open_in_app", "import_board",
        "pick_in_viewport", "ask_user", "analyze_structure", "check_interference", "agent_turn",
    ]

    func testEveryInventoryToolIsListedAndCallableInTheApp() async throws {
        let rows = try inventory()
        XCTAssertGreaterThan(rows.count, 200)
        let expected = Set(rows.filter { !$0.1.hasPrefix("node proxy") }.map(\.0))
        let m = EditorModel()
        let listed = Set(await coreTools(m).keys)
        XCTAssertEqual(expected.subtracting(listed), [], "inventory tools missing from the app's list")
        XCTAssertEqual(listed.subtracting(expected), [], "listed tools missing from the inventory")

        for name in expected.subtracting(Self.needsWindow).sorted() {
            let r = await coreCall(m, name, [:])
            let text = resultText(r)
            XCTAssertFalse(text.contains("Unknown tool") || text.hasPrefix("unknown tool"), "\(name): \(text)")
        }
        AgentTurn.shared.end()
    }

    func testEveryHostToolHasASwiftImplementation() async {
        let m = EditorModel()
        AgentCore.shared.attach(m)
        for d in AgentCore.hostDefinitions {
            let name = d["name"] as? String ?? ""
            let routed = ProofAgentTools.toolNames.contains(name) || AnalysisAgentTools.toolNames.contains(name)
                || AgentViewer.toolNames.contains(name) || AgentEdits.hostToolNames.contains(name)
                || AgentTools.viewportToolNames.contains(name) || name == "agent_turn"
            XCTAssertTrue(routed, "host tool \(name) has no Swift implementation")
        }
        XCTAssertEqual(AgentCore.hostDefinitions.count, 33)
    }

    func testResourcesComeFromTheLiveWindow() async throws {
        let m = EditorModel()
        AgentCore.shared.attach(m)
        _ = await coreCall(m, "create", ["type": "cube", "name": "Box", "params": ["size": ["x": 1, "y": 1, "z": 1]]])
        let list = await AgentCore.shared.rpc("resources/list")
        let uris = Set((((list["result"] as? [String: Any])?["resources"] as? [[String: Any]]) ?? []).compactMap { $0["uri"] as? String })
        XCTAssertTrue(uris.isSuperset(of: ["vcad://doc/current", "vcad://doc/selection", "vcad://doc/view", "ui://vcad/viewer"]), "\(uris)")
        let read = await AgentCore.shared.rpc("resources/read", ["uri": "vcad://doc/current"])
        let text = (((read["result"] as? [String: Any])?["contents"] as? [[String: Any]])?.first?["text"] as? String) ?? ""
        XCTAssertTrue(text.contains("\"Box\""), text)
        XCTAssertTrue(text.contains("\"revision\""), "the app's wrapper (revision, editable) is kept")
    }
}
