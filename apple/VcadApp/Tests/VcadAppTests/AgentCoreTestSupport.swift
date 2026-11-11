import XCTest
@testable import VcadApp

/// Drive a tool exactly as an agent does: through the Rust MCP core the app
/// hosts (AgentCore → crates/vcad-mcp), in-process, against `m`.
@MainActor
func coreCall(_ m: EditorModel, _ name: String, _ args: [String: Any] = [:],
              inputResponses: [String: Any]? = nil) async -> [String: Any] {
    AgentCore.shared.attach(m)
    // No one is at the window to Apply/Reject a ghost review in tests.
    if AgentGhost.shared.autoDecision == nil { AgentGhost.shared.autoDecision = true }
    return await AgentCore.shared.call(name, args, inputResponses: inputResponses)
}

/// The first text block of a tool result.
func resultText(_ r: [String: Any]) -> String {
    ((r["content"] as? [[String: Any]])?.first?["text"] as? String) ?? ""
}

/// Every tool the core lists, by name.
@MainActor
func coreTools(_ m: EditorModel) async -> [String: [String: Any]] {
    AgentCore.shared.attach(m)
    let r = await AgentCore.shared.rpc("tools/list")
    let tools = (r["result"] as? [String: Any])?["tools"] as? [[String: Any]] ?? []
    return Dictionary(tools.compactMap { t in (t["name"] as? String).map { ($0, t) } }, uniquingKeysWith: { a, _ in a })
}
