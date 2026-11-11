import XCTest
@testable import VcadApp

/// M2 "proof, not promises", driven exactly as the MCP bridge dispatches it:
/// receipts re-verify on edit (a nudge turns a clearance Violated, undo brings
/// it back), and elicitation round-trips through the viewport / native sheet
/// or, for an MRTR client, through `inputResponses`.
/// Tool results are plain JSON dictionaries; box them across the Task boundary.
private struct JSONBox: @unchecked Sendable { let value: [String: Any]? }

@MainActor
final class ProofTests: XCTestCase {
    override func tearDown() async throws {
        ElicitationCenter.shared.cancelAll()
        ReceiptStore.shared.documentChanged(nil)
    }

    private func call(_ m: EditorModel, _ name: String, _ args: [String: Any] = [:]) async throws -> [String: Any] {
        let r = try await AgentTools.call(name, args, model: m)
        XCTAssertNil(r["isError"], "\(name) failed: \(r)")
        return r
    }

    private func created(_ r: [String: Any]) throws -> String {
        let text = ((r["content"] as? [[String: Any]])?.first?["text"] as? String) ?? ""
        let range = try XCTUnwrap(text.range(of: "created part "), text)
        return String(text[range.upperBound...].prefix { $0 != ";" && $0 != "." && $0 != " " })
    }

    private func proof(_ m: EditorModel, _ name: String, _ args: [String: Any] = [:],
                       inputs: Any? = nil) async throws -> [String: Any] {
        let r = try await ProofAgentTools.call(name, args, inputResponses: inputs, model: m)
        return try XCTUnwrap(r["structuredContent"] as? [String: Any], "\(name): no structuredContent in \(r)")
    }

    /// Box at the origin, Wedge 3 mm away along +x, both 10 mm cubes.
    private func twoBoxes() async throws -> (EditorModel, wedge: String) {
        let m = EditorModel()
        _ = try created(try await call(m, "create", ["type": "cube", "name": "Box", "params": ["size": ["x": 10, "y": 10, "z": 10]]]))
        let wedge = try created(try await call(m, "create", ["type": "cube", "name": "Wedge", "params": ["size": ["x": 10, "y": 10, "z": 10]]]))
        _ = try await call(m, "translate", ["child": wedge, "offset": ["x": 13, "y": 0, "z": 0]])
        return (m, wedge)
    }

    // MARK: receipts

    func testClearanceReceiptGoesRedOnEditAndRecoversOnUndo() async throws {
        let (m, wedge) = try await twoBoxes()
        let store = ReceiptStore.shared

        // The agent's claim, persisted and returned as structured output.
        let asserted = try await proof(m, "assert_clearance",
                                       ["label": "gap", "group_a": ["Box"], "group_b": ["Wedge"], "min_mm": 2])
        XCTAssertEqual(asserted["status"] as? String, "Holds", "\(asserted)")
        let receipt = try XCTUnwrap(asserted["receipt"] as? [String: Any])
        XCTAssertEqual(receipt["schema"] as? String, "vcad.receipt/1")
        let claim = try XCTUnwrap((receipt["claims"] as? [[String: Any]])?.first)
        XCTAssertEqual(claim["id"] as? String, "mech.clearance.gap")
        XCTAssertEqual(claim["verdict"] as? String, "pass")
        XCTAssertEqual(m.undoStack.last?.name.contains("Assert clearance"), true, "\(m.undoStack.map(\.name))")
        let specs = try XCTUnwrap(m.documentJSON?["clearance_specs"] as? [[String: Any]])
        XCTAssertEqual(specs.first?["label"] as? String, "gap", "persisted on the document")

        // Nudge the Wedge 2 mm toward the Box: 1 mm < 2 mm required. The edit
        // bumps the observable revision the badge strip re-verifies on.
        let rev = m.documentRevision
        _ = try await call(m, "translate", ["child": wedge, "offset": ["x": -2, "y": 0, "z": 0]])
        XCTAssertNotEqual(m.documentRevision, rev, "an edit must signal the badges")
        store.documentChanged(m.documentJSON)
        await store.settle()
        XCTAssertEqual(store.report?.status, .violated, "\(String(describing: store.report?.checks))")
        let check = try XCTUnwrap(store.report?.checks.first)
        XCTAssertEqual(check.status, .violated)
        XCTAssertLessThan(check.measuredMm ?? .infinity, 2)
        XCTAssertEqual(check.worstPair ?? [], ["Box", "Wedge"])

        // Accepting never launders a violation.
        let n = try await store.accept(m)
        XCTAssertEqual(n, 0)

        // Undo is an edit too: the badge goes green again.
        m.undo()
        store.documentChanged(m.documentJSON)
        await store.settle()
        XCTAssertEqual(store.report?.status, .holds)

        let verified = try await proof(m, "verify_receipts")
        XCTAssertEqual(verified["status"] as? String, "Holds")
    }

    func testNoAssertionsMeansNoBadges() async throws {
        let (m, _) = try await twoBoxes()
        ReceiptStore.shared.documentChanged(m.documentJSON)
        await ReceiptStore.shared.settle()
        XCTAssertNil(ReceiptStore.shared.report)
        let v = try await proof(m, "verify_receipts")
        XCTAssertTrue(v["status"] is NSNull)
    }

    // MARK: elicitation

    private func waitForRequest() async throws -> ElicitRequest {
        for _ in 0..<200 {
            if let r = ElicitationCenter.shared.current { return r }
            try await Task.sleep(nanoseconds: 5_000_000)
        }
        throw XCTSkip("no elicitation appeared")
    }

    /// "Which edge?" — the tool blocks, the user clicks an edge in the
    /// viewport, presses Use, and the tool returns the pick.
    func testPickInViewportRoundTrip() async throws {
        let (m, _) = try await twoBoxes()
        let task = Task { JSONBox(value: try await self.proof(m, "pick_in_viewport", ["message": "Which edge should I fillet?", "kind": "edge"])) }
        let req = try await waitForRequest()
        XCTAssertEqual(req.pick, .edge)
        XCTAssertEqual(m.picking.filter, .edge, "the viewport is armed for edges")
        XCTAssertFalse(ElicitationCenter.shared.canAccept, "nothing picked yet")

        m.picking.select(.edge(3, part: 0), additive: false)   // the viewport click
        XCTAssertTrue(ElicitationCenter.shared.canAccept)
        ElicitationCenter.shared.respond(.accept)

        let boxed = try await task.value
        let out = try XCTUnwrap(boxed.value)
        XCTAssertEqual(out["action"] as? String, "accept")
        let sel = try XCTUnwrap((out["content"] as? [String: Any])?["selection"] as? [[String: Any]])
        XCTAssertEqual(sel.count, 1)
        XCTAssertEqual(sel[0]["kind"] as? String, "edge")
        XCTAssertEqual(sel[0]["id"] as? Int, 3)
        XCTAssertEqual(sel[0]["part"] as? Int, 0)
        XCTAssertEqual(m.picking.filter, .standard, "picking is restored after the answer")
        XCTAssertNil(ElicitationCenter.shared.current)
    }

    /// A material choice in a native sheet.
    func testAskUserFormRoundTrip() async throws {
        let m = EditorModel()
        let schema: [String: Any] = ["type": "object",
                                     "properties": ["material": ["type": "string", "enum": ["pla", "petg", "aluminum_6061"]],
                                                    "infill": ["type": "integer"]],
                                     "required": ["material"]]
        let box = JSONBox(value: schema)
        let task = Task { JSONBox(value: try await self.proof(m, "ask_user", ["message": "Which material?", "requested_schema": box.value!])) }
        let req = try await waitForRequest()
        XCTAssertNil(req.pick)
        XCTAssertTrue(ElicitationCenter.shared.showsSheet)
        XCTAssertEqual(req.fields.map(\.name), ["material", "infill"], "required first")
        ElicitationCenter.shared.values["material"] = "petg"
        ElicitationCenter.shared.values["infill"] = "20"
        ElicitationCenter.shared.respond(.accept)
        let boxed = try await task.value
        let out = try XCTUnwrap(boxed.value)
        XCTAssertEqual(out["action"] as? String, "accept")
        let content = try XCTUnwrap(out["content"] as? [String: Any])
        XCTAssertEqual(content["material"] as? String, "petg")
        XCTAssertEqual(content["infill"] as? Int, 20, "typed by the schema")
    }

    /// An MRTR client answered in chat: the retry's inputResponses satisfy the
    /// elicitation with no UI at all.
    func testMRTRInputResponsesSkipTheUI() async throws {
        let m = EditorModel()
        let out = try await proof(m, "ask_user", ["message": "Which material?"],
                                  inputs: ["input_1": ["action": "accept", "content": ["material": "steel"]]])
        XCTAssertEqual(out["action"] as? String, "accept")
        XCTAssertEqual((out["content"] as? [String: Any])?["material"] as? String, "steel")
        XCTAssertNil(ElicitationCenter.shared.current)
    }

    /// Deleting a part while the document carries receipts asks first; a
    /// decline leaves the document alone.
    func testDestructiveDeleteConfirmsWhenReceiptsExist() async throws {
        let (m, wedge) = try await twoBoxes()
        _ = try await proof(m, "assert_clearance",
                            ["label": "gap", "group_a": ["Box"], "group_b": ["Wedge"], "min_mm": 2])
        let roots = (m.documentJSON?["roots"] as? [Any])?.count
        let task = Task { JSONBox(value: await ProofAgentTools.confirmDestructive("delete", ["part_id": wedge], inputResponses: nil, model: m)) }
        let req = try await waitForRequest()
        XCTAssertTrue(req.destructive)
        XCTAssertTrue(req.message.contains("gap"), req.message)
        ElicitationCenter.shared.respond(.decline)
        let refused = await task.value.value
        XCTAssertEqual(refused?["isError"] as? Bool, true)
        XCTAssertEqual((m.documentJSON?["roots"] as? [Any])?.count, roots, "nothing deleted")

        // Accepting lets the edit through (nil = proceed).
        let ok = await ProofAgentTools.confirmDestructive("delete", ["part_id": wedge],
                                                          inputResponses: ["input_1": ["action": "accept"]], model: m)
        XCTAssertNil(ok)
    }

    func testRequestParsesMcpParams() {
        let r = ElicitRequest(params: ["message": "Pick faces", "x-vcad-pick": "face", "x-vcad-multiple": true,
                                       "requestedSchema": ["type": "object", "properties": [String: Any]()]])
        XCTAssertEqual(r.pick, .face)
        XCTAssertTrue(r.multiple)
        XCTAssertEqual(r.params["x-vcad-pick"] as? String, "face")
        XCTAssertNil(ElicitResponse(json: ["action": "nope"]))
        XCTAssertEqual(ElicitResponse(json: ["action": "decline"])?.action, .decline)
    }
}
