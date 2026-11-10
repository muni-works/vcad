import Foundation

// Transport for the AI intent bar. It used to call vcad.io's hosted /api/chat;
// vcad no longer operates hosted services, so the client fails closed. Agents
// drive vcad through the local MCP server (`npx -y @vcad/mcp`) instead.

enum IntentError: LocalizedError {
    case notAvailable

    var errorDescription: String? {
        switch self {
        case .notAvailable:
            return "AI authoring is not available in this build. Connect your own agent with the local MCP server: npx -y @vcad/mcp"
        }
    }
}

/// Fail-closed stand-in for the retired hosted AI endpoint. Makes no network
/// calls.
struct VcadIntentClient {
    func complete(system: String, user: String) async throws -> String {
        throw IntentError.notAvailable
    }
}
