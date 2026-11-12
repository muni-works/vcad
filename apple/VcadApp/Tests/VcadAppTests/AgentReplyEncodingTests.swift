import XCTest
@testable import VcadApp

/// Agent replies splice ready-made document bytes instead of re-serializing
/// a dictionary on the main thread.
final class AgentReplyEncodingTests: XCTestCase {
    private func parse(_ s: String) throws -> [String: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: Data(s.utf8)) as? [String: Any], s)
    }

    func testRawDocumentIsSplicedAsJSON() throws {
        let doc = Data(#"{"version":"0.1","nodes":{},"roots":[]}"#.utf8)
        let a = try parse(AgentCore.encode(["id": "w#1", AgentCore.rawDocumentKey: doc]))
        XCTAssertEqual(a["id"] as? String, "w#1")
        XCTAssertEqual((a["document"] as? [String: Any])?["version"] as? String, "0.1")
        let b = try parse(AgentCore.encode([AgentCore.rawDocumentKey: doc]))
        XCTAssertEqual(b.count, 1)
        XCTAssertNotNil(b["document"] as? [String: Any])
        let c = try parse(AgentCore.encode(["error": "x"]))
        XCTAssertEqual(c["error"] as? String, "x")
    }
}
