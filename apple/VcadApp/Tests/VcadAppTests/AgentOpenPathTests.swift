import XCTest
@testable import VcadApp

/// Fuzz finding F3 (docs/qa/fuzz-2026-11-12.md): the agent bridge's
/// `open_path` opened any file — `/etc/passwd` became the window's document
/// and Save target. It must open only existing vcad documents.
final class AgentOpenPathTests: XCTestCase {
    func testRefusesNonDocuments() throws {
        XCTAssertNil(AgentCore.openableDocument("/etc/passwd"))
        XCTAssertNil(AgentCore.openableDocument("../../etc/passwd"))
        XCTAssertNil(AgentCore.openableDocument(""))
        XCTAssertNil(AgentCore.openableDocument("/nonexistent/x.vcad"))
        let txt = FileManager.default.temporaryDirectory.appendingPathComponent("notes-\(UUID()).txt")
        try "hi".write(to: txt, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: txt) }
        XCTAssertNil(AgentCore.openableDocument(txt.path))
    }

    func testAcceptsDocumentsAndFolders() throws {
        let doc = FileManager.default.temporaryDirectory.appendingPathComponent("part-\(UUID()).vcad")
        try "{}".write(to: doc, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: doc) }
        XCTAssertEqual(AgentCore.openableDocument(doc.path), doc.standardizedFileURL)
        XCTAssertNotNil(AgentCore.openableDocument(FileManager.default.temporaryDirectory.path))
    }
}
