import XCTest
@testable import VcadApp

/// `vcad://open?…` parsing and hand-off (docs/launch/vcad-url-scheme.md).
final class DeepLinkTests: XCTestCase {
    private func link(_ s: String) -> DeepLink? { DeepLink(URL(string: s)!) }

    func testLocalPath() {
        XCTAssertEqual(link("vcad://open?path=/tmp/a%20b.vcad"), .file(URL(fileURLWithPath: "/tmp/a b.vcad")))
        XCTAssertEqual(link("vcad://open?path=/tmp/x.loon"), .file(URL(fileURLWithPath: "/tmp/x.loon")))
    }

    func testRejectsUnsafePaths() {
        XCTAssertNil(link("vcad://open?path=relative.vcad"))
        XCTAssertNil(link("vcad://open?path=/etc/passwd"))
        XCTAssertNil(link("vcad://open?session=../../etc/passwd"))
        XCTAssertNil(link("vcad://open?session=doc_1&dir=relative"))
        XCTAssertNil(link("https://open?path=/tmp/a.vcad"))
        XCTAssertNil(link("vcad://delete?path=/tmp/a.vcad"))
        XCTAssertNil(link("vcad://open"))
    }

    func testSessionLink() {
        XCTAssertEqual(
            link("vcad://open?session=doc_10_abc-DEF&dir=/tmp/s&name=Bracket"),
            .session(id: "doc_10_abc-DEF", dir: URL(fileURLWithPath: "/tmp/s", isDirectory: true), name: "Bracket"))
    }

    func testSessionMaterializesAPrivateCopy() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("deeplink-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let json = #"{"version":"0.1","nodes":{},"roots":[]}"#
        try Data(json.utf8).write(to: dir.appendingPathComponent("doc_1.json"))
        let url = try XCTUnwrap(link("vcad://open?session=doc_1&dir=\(dir.path)&name=My%20Part")).materialize()
        XCTAssertEqual(url.lastPathComponent, "My Part.vcad")
        XCTAssertNotEqual(url.deletingLastPathComponent().path, dir.path)
        XCTAssertEqual(try String(contentsOf: url, encoding: .utf8), json)
    }

    func testInlineGzipBase64url() throws {
        // `{"version":"0.1","nodes":{},"roots":[]}` gzipped (node zlib.gzipSync) + base64url.
        let doc = "H4sIAAAAAAAAE6tWKkstKs7Mz1OyUjLQM1TSUcrLT0ktVrKqrtVRKsrPLwEyo2NrARNWNg0nAAAA"
        let l = try XCTUnwrap(link("vcad://open?doc=\(doc)&name=Shared"))
        guard case .inline(let data, let name) = l else { return XCTFail("not inline") }
        XCTAssertEqual(name, "Shared")
        XCTAssertEqual(String(decoding: data, as: UTF8.self), #"{"version":"0.1","nodes":{},"roots":[]}"#)
        XCTAssertEqual(try l.materialize().pathExtension, "vcad")
    }

    func testInlineNonDocumentIsRefused() {
        let notJSON = DeepLink.inline(Data("hello".utf8), name: nil)
        XCTAssertThrowsError(try notJSON.materialize())
    }
}
