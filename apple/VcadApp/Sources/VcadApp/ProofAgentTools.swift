import Foundation

// Proof tools on the app's MCP server (AgentBridge.swift), M2:
//
// - Receipts. `assert_clearance` persists a claim on the document,
//   `verify_receipts` re-measures every persisted claim, `accept_receipts`
//   re-baselines. Every result carries the receipt as MCP structured output
//   (`structuredContent`, declared by `outputSchema`): status per claim plus
//   the unified `vcad.receipt/1` DesignReceipt — machine-checkable, not prose.
//   The same claims drive the live badges the user sees.
//
// - Elicitation. `pick_in_viewport` and `ask_user` are `elicitation/create`
//   answered natively: the user clicks the edge/face/part in the viewport, or
//   fills a native sheet. A client that speaks MRTR (SEP-2322) may answer in
//   chat instead by retrying with `inputResponses: {input_1: {action, content}}`
//   — the UI is skipped. `delete` asks for a destructive confirm in-app when
//   the document carries receipts the deletion could break.

@MainActor
enum ProofAgentTools {
    typealias Failure = AgentTools.Failure

    private static func schema(_ props: [String: Any], required: [String] = []) -> [String: Any] {
        ["type": "object", "properties": props, "required": required]
    }

    /// `outputSchema` shared by the receipt tools.
    static let receiptOutputSchema: [String: Any] = [
        "type": "object",
        "properties": [
            "status": ["type": ["string", "null"], "enum": ["Holds", "Stale", "Violated", NSNull()],
                       "description": "Worst status across claims; null when nothing is asserted."],
            "checks": ["type": "array", "items": [
                "type": "object",
                "properties": [
                    "label": ["type": "string"], "claim_id": ["type": "string"],
                    "status": ["type": "string", "enum": ["Holds", "Stale", "Violated"]],
                    "subject": ["type": "string"], "required_mm": ["type": "number"],
                    "measured_mm": ["type": "number"], "baseline_mm": ["type": "number"],
                    "worst_pair": ["type": "array", "items": ["type": "string"]],
                    "reason": ["type": "string"],
                ],
                "required": ["label", "claim_id", "status", "subject", "required_mm"],
            ]],
            "receipt": ["type": "object", "description": "Unified vcad.receipt/1 DesignReceipt."],
        ],
        "required": ["status", "checks", "receipt"],
    ]

    static let definitions: [[String: Any]] = [
        ["name": "assert_clearance",
         "description": "Persist a clearance claim on the open document: the minimum distance between part group_a and group_b (part names or root node ids) must stay ≥ min_mm. It becomes a live badge in the user's viewport that re-verifies on every edit (Holds green / Stale gray / Violated red). Returns the receipt as structured output. Re-asserting a label replaces it. One undo step.",
         "inputSchema": schema(["label": ["type": "string"],
                                "group_a": ["type": "array", "items": ["type": "string"]],
                                "group_b": ["type": "array", "items": ["type": "string"]],
                                "min_mm": ["type": "number"],
                                "allow_contact": ["type": "boolean", "description": "Flush contact (distance ≈ 0) satisfies the claim."]],
                               required: ["label", "group_a", "group_b", "min_mm"]),
         "outputSchema": receiptOutputSchema],
        ["name": "verify_receipts",
         "description": "Re-measure every claim persisted on the open document against the current geometry. Violated = fails or can't be measured (fail-closed); Stale = holds but drifted from its accepted measurement; Holds = holds and matches. Returns per-claim status and the unified DesignReceipt as structured output. Never report a claim as true without this.",
         "inputSchema": schema([:]),
         "outputSchema": receiptOutputSchema],
        ["name": "accept_receipts",
         "description": "Accept the current measurements as the baseline for holding claims (label, or all), turning Stale back to Holds. A Violated claim is never accepted. One undo step.",
         "inputSchema": schema(["label": ["type": "string"]]),
         "outputSchema": receiptOutputSchema],
        ["name": "pick_in_viewport",
         "description": "Ask the user to pick geometry by clicking it in the viewport (MCP elicitation answered natively). kind: edge | face | part; multiple allows several. Blocks until the user answers. Returns {action: accept|decline|cancel, content: {selection: [{kind, part, id, selector?}]}} — selector is a durable IR EdgeQuery/FaceRef to use in edits; ids are session handles.",
         "inputSchema": schema(["message": ["type": "string", "description": "The question, e.g. \"Which edge should I fillet?\""],
                                "kind": ["type": "string", "enum": ["edge", "face", "part"]],
                                "multiple": ["type": "boolean"]],
                               required: ["message", "kind"]),
         "outputSchema": ["type": "object", "properties": ["action": ["type": "string"], "content": ["type": "object"]],
                          "required": ["action"]]],
        ["name": "ask_user",
         "description": "Ask the user a question with a native sheet (MCP elicitation). requested_schema is an MCP elicitation schema: a flat object of string (optionally enum + enumNames) / number / integer / boolean properties — e.g. a material choice. destructive: true styles it as a confirm before a destructive edit. Returns {action, content}.",
         "inputSchema": schema(["message": ["type": "string"],
                                "requested_schema": ["type": "object"],
                                "destructive": ["type": "boolean"]],
                               required: ["message"]),
         "outputSchema": ["type": "object", "properties": ["action": ["type": "string"], "content": ["type": "object"]],
                          "required": ["action"]]],
    ]
    static var toolNames: Set<String> { Set(definitions.compactMap { $0["name"] as? String }) }

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
        case "assert_clearance":
            guard let label = a["label"] as? String, !label.isEmpty,
                  let ga = a["group_a"] as? [String], !ga.isEmpty,
                  let gb = a["group_b"] as? [String], !gb.isEmpty,
                  let min = (a["min_mm"] as? NSNumber)?.doubleValue else {
                throw Failure(message: "label, group_a, group_b and min_mm are required")
            }
            let r = try await ReceiptStore.shared.assertClearance(
                m, label: label, groupA: ga, groupB: gb, minMm: min, allowContact: a["allow_contact"] as? Bool ?? false)
            return try receiptResult(r, ReceiptStore.shared.error)
        case "verify_receipts":
            return try receiptResult(await ReceiptStore.shared.refresh(m), ReceiptStore.shared.error)
        case "accept_receipts":
            let n = try await ReceiptStore.shared.accept(m, label: a["label"] as? String)
            var out = try receiptResult(ReceiptStore.shared.report, ReceiptStore.shared.error)
            out["content"] = [["type": "text", "text": "Accepted \(n) claim\(n == 1 ? "" : "s").\n"
                                + ((out["content"] as? [[String: Any]])?.first?["text"] as? String ?? "")]]
            return out
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
    static func confirmDestructive(_ name: String, _ a: [String: Any], inputResponses: Any?,
                                   model m: EditorModel) async -> [String: Any]? {
        guard name == "delete", let checks = ReceiptStore.shared.report?.checks, !checks.isEmpty else { return nil }
        var inputs = InputResponses(inputResponses)
        let part = a["part_id"] as? String ?? "a part"
        let labels = checks.map(\.label).joined(separator: ", ")
        let r = ElicitRequest(
            message: "The agent wants to delete \(part). This document has \(checks.count) receipt\(checks.count == 1 ? "" : "s") (\(labels)) that may stop verifying.",
            schema: ["type": "object", "properties": [String: Any]()], destructive: true)
        let resp = await elicit(r, &inputs, model: m)
        guard resp.action != .accept else { return nil }
        return AgentTools.error("The user \(resp.action == .decline ? "declined" : "cancelled") deleting \(part) — nothing changed.")
    }

    /// A tool result with `structuredContent` and its JSON mirrored as text
    /// (for clients without structured output).
    static func structured(_ obj: [String: Any]) -> [String: Any] {
        var r = AgentTools.json(obj)
        r["structuredContent"] = obj
        return r
    }

    static func receiptResult(_ report: ReceiptReport?, _ error: String?) throws -> [String: Any] {
        if let error, report == nil { throw Failure(message: "Receipt verification failed: \(error)") }
        guard let report else {
            return structured(["status": NSNull(), "checks": [Any](),
                               "receipt": ["schema": "vcad.receipt/1", "claims": [Any]()]])
        }
        let checks = (try? JSONSerialization.jsonObject(with: JSONEncoder().encode(report.checks))) ?? []
        return structured(["status": report.status?.rawValue ?? NSNull(), "checks": checks, "receipt": report.receipt.any])
    }
}
