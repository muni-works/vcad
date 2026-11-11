import Foundation

// Proof host tools (M2). Receipts (`assert_clearance`, `verify_receipts`,
// `accept_receipts`) are native in the MCP core (crates/vcad-mcp/src/tools/
// proof.rs); the live badges follow the document they persist on.
//
// - Elicitation. `pick_in_viewport` and `ask_user` are `elicitation/create`
//   answered natively: the user clicks the edge/face/part in the viewport, or
//   fills a native sheet. A client that speaks MRTR (SEP-2322) may answer in
//   chat instead by retrying with `inputResponses: {input_1: {action, content}}`
//   — the UI is skipped. The core's `delete` asks for a destructive confirm
//   in-app (AgentCore `__confirm_destructive`) when the document carries
//   receipts the deletion could break.

@MainActor
enum ProofAgentTools {
    typealias Failure = AgentTools.Failure

    private static func schema(_ props: [String: Any], required: [String] = []) -> [String: Any] {
        ["type": "object", "properties": props, "required": required]
    }

    /// `outputSchema` shared by the receipt tools.

    /// Elicitation host tools (listed by the core, executed here).
    static let toolNames: Set<String> = ["pick_in_viewport", "ask_user"]

    /// Answers a client already gave over MRTR, keyed `input_1`, `input_2`, …
    /// in the order a call raises them.
    struct InputResponses {
        var provided: [String: [String: Any]]
        private var seq = 0
        init(_ raw: Any?) { provided = raw as? [String: [String: Any]] ?? [:] }
        mutating func next() -> ElicitResponse? {
            seq += 1
            return provided["input_\(seq)"].flatMap(ElicitResponse.init(json:))
        }
    }

    /// Elicit: from MRTR `inputResponses` when present, else natively.
    static func elicit(_ r: ElicitRequest, _ inputs: inout InputResponses, model m: EditorModel) async -> ElicitResponse {
        if let given = inputs.next() { return given }
        return await ElicitationCenter.shared.request(r, model: m)
    }

    static func call(_ name: String, _ a: [String: Any], inputResponses: Any?, model m: EditorModel) async throws -> [String: Any] {
        var inputs = InputResponses(inputResponses)
        switch name {
        case "pick_in_viewport":
            guard let msg = a["message"] as? String,
                  let kind = (a["kind"] as? String).flatMap(ElicitPick.init(rawValue:)) else {
                throw Failure(message: "message and kind (edge | face | part) are required")
            }
            let resp = await elicit(ElicitRequest(message: msg, pick: kind, multiple: a["multiple"] as? Bool ?? false), &inputs, model: m)
            return structured(resp.json)
        case "ask_user":
            guard let msg = a["message"] as? String else { throw Failure(message: "message is required") }
            let r = ElicitRequest(message: msg, schema: a["requested_schema"] as? [String: Any],
                                  destructive: a["destructive"] as? Bool ?? false)
            return structured(await elicit(r, &inputs, model: m).json)
        default:
            throw Failure(message: "unknown tool \(name)")
        }
    }

    /// Before a destructive agent edit: when the document carries receipts,
    /// confirm in-app (or via MRTR). Returns a refusal result when the user
    /// says no, nil to proceed.

    /// A tool result with `structuredContent` and its JSON mirrored as text
    /// (for clients without structured output).
    static func structured(_ obj: [String: Any]) -> [String: Any] {
        var r = AgentTools.json(obj)
        r["structuredContent"] = obj
        return r
    }

}
