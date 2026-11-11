import Foundation
import CVcadFFI
import Observation

// Live receipts (M2 "proof, not promises"): every clearance assertion the
// agent (or the user) persisted on the document — `clearance_specs` — is
// re-verified against the current geometry after every edit, by the same
// kernel the MCP server uses (crates/vcad-ffi/src/receipts.rs). The badge
// strip (ReceiptBadges.swift) draws the result: Holds green, Stale gray,
// Violated red. Nudge a part inside an assertion and its badge turns red on
// the next evaluation.
//
// The kernel work runs off the main actor on a snapshot of the document
// bytes; a result whose bytes have since changed is dropped, never drawn
// over geometry it no longer describes.

/// Per-assertion status (the kernel's `ReceiptStatus`).
enum ReceiptStatus: String, Codable, Comparable {
    case holds = "Holds", stale = "Stale", violated = "Violated"

    private var rank: Int { switch self { case .holds: 0; case .stale: 1; case .violated: 2 } }
    static func < (a: Self, b: Self) -> Bool { a.rank < b.rank }
}

/// One re-verified assertion.
struct ReceiptCheck: Codable, Identifiable, Equatable {
    var id: String { claimId }
    let label: String
    let claimId: String
    let status: ReceiptStatus
    let subject: String
    let requiredMm: Double
    let measuredMm: Double?
    let baselineMm: Double?
    let worstPair: [String]?
    let points: [[Double]]?
    let reason: String?

    enum CodingKeys: String, CodingKey {
        case label, status, subject, reason, points
        case claimId = "claim_id", requiredMm = "required_mm", measuredMm = "measured_mm"
        case baselineMm = "baseline_mm", worstPair = "worst_pair"
    }

    /// "≥ 2 mm · 3.00 mm" — the badge's numeric line.
    var summary: String {
        let req = "≥ \(Self.mm(requiredMm))"
        guard let m = measuredMm else { return req }
        return "\(req) · \(String(format: "%.2f", m)) mm"
    }

    static func mm(_ v: Double) -> String {
        v == v.rounded() ? "\(Int(v)) mm" : String(format: "%g mm", v)
    }
}

/// Whole-document verification (the kernel's `ReceiptReport`).
struct ReceiptReport: Decodable {
    let status: ReceiptStatus?
    let checks: [ReceiptCheck]
    /// The unified DesignReceipt, kept as JSON: tools return it verbatim as
    /// structured output.
    let receipt: JSONValue
}

enum ReceiptFFI {
    struct Failure: Error, CustomStringConvertible {
        let message: String
        var description: String { message }
    }

    /// Re-verify `doc`'s persisted assertions.
    static func verify(doc: Data) throws -> ReceiptReport {
        let str = String(decoding: doc, as: UTF8.self)
        guard let raw = str.withCString({ vcad_receipts_verify($0) }) else {
            throw Failure(message: AnalysisFFI.lastError() ?? "receipt verification failed")
        }
        defer { vcad_string_free(raw) }
        return try JSONDecoder().decode(ReceiptReport.self, from: Data(bytes: raw, count: strlen(raw)))
    }

    /// Re-baseline holding assertions (`label` nil = all). Returns the patched
    /// document and how many were accepted.
    static func accept(doc: Data, label: String?) throws -> (document: [String: Any], accepted: Int) {
        let req = try JSONSerialization.data(withJSONObject: label.map { ["label": $0] } ?? [:])
        let out = try AnalysisFFI.call(vcad_receipts_accept, doc: doc, request: req)
        guard let obj = try JSONSerialization.jsonObject(with: out) as? [String: Any],
              let d = obj["document"] as? [String: Any] else {
            throw Failure(message: "accept returned no document")
        }
        return (d, obj["accepted"] as? Int ?? 0)
    }
}

@MainActor @Observable
final class ReceiptStore {
    static let shared = ReceiptStore()

    /// Latest verification of the open document (nil = nothing asserted).
    private(set) var report: ReceiptReport?
    /// A verification is running for bytes newer than `report`.
    private(set) var verifying = false
    private(set) var error: String?
    /// Which badge is expanded.
    var expanded: String?

    /// Bytes the in-flight / latest verification is for.
    @ObservationIgnored private var key: Data?
    @ObservationIgnored private var task: Task<Void, Never>?
    /// Swappable for tests.
    @ObservationIgnored var verifier: @Sendable (Data) throws -> ReceiptReport = { try ReceiptFFI.verify(doc: $0) }

    /// The document changed: re-verify. Cheap when nothing is asserted (no
    /// kernel call), and coalesces a burst of edits to the latest bytes.
    func documentChanged(_ doc: [String: Any]?) {
        guard let doc, let specs = doc["clearance_specs"] as? [Any], !specs.isEmpty else {
            task?.cancel(); task = nil; key = nil
            report = nil; verifying = false; error = nil
            return
        }
        guard let data = try? JSONSerialization.data(withJSONObject: doc, options: [.sortedKeys]),
              data != key else { return }
        key = data
        verifying = true
        task?.cancel()
        let verify = verifier
        task = Task { [weak self] in
            let result = await Task.detached(priority: .userInitiated) { () -> Result<ReceiptReport, Error> in
                Result { try verify(data) }
            }.value
            guard let self, !Task.isCancelled, self.key == data else { return }
            switch result {
            case .success(let r): self.report = r; self.error = nil
            case .failure(let e): self.error = "\(e)"
            }
            self.verifying = false
        }
    }

    /// Wait for the in-flight verification (tests, agent tools).
    func settle() async { await task?.value }

    /// Verify the model's document now and wait for it.
    func refresh(_ m: EditorModel) async -> ReceiptReport? {
        documentChanged(m.documentJSON)
        await settle()
        return report
    }

    /// Accept the current measurements as the baselines (one named undo
    /// step). A Violated assertion is never accepted.
    @discardableResult
    func accept(_ m: EditorModel, label: String? = nil) async throws -> Int {
        guard let data = AnalysisStore.documentData(m) else { throw ReceiptFFI.Failure(message: "No document is open.") }
        let (doc, n) = try ReceiptFFI.accept(doc: data, label: label)
        if n > 0 {
            _ = try AgentEdits.edit(m, name: label.map { "Accept receipt “\($0)”" } ?? "Accept receipts") { try $0.loadIR(doc) }
        }
        await refresh(m)
        return n
    }

    /// Persist a clearance assertion on the document (one named undo step).
    func assertClearance(_ m: EditorModel, label: String, groupA: [String], groupB: [String],
                         minMm: Double, allowContact: Bool = false) async throws -> ReceiptReport? {
        guard var doc = m.documentJSON else { throw ReceiptFFI.Failure(message: "No document is open.") }
        var specs = (doc["clearance_specs"] as? [[String: Any]]) ?? []
        specs.removeAll { ($0["label"] as? String) == label }
        var spec: [String: Any] = ["label": label, "group_a": groupA, "group_b": groupB, "min_mm": minMm]
        if allowContact { spec["allow_contact"] = true }
        specs.append(spec)
        doc["clearance_specs"] = specs
        _ = try AgentEdits.edit(m, name: "Assert clearance “\(label)”") { try $0.loadIR(doc) }
        return await refresh(m)
    }
}
