import XCTest
@testable import VcadApp

final class FeedbackTests: XCTestCase {
    let env = FeedbackEnvironment(appVersion: "1.0.0", build: "42", macOS: "26.1.0",
                                  chip: "Apple M3 Pro", workspace: "design")

    private func query(_ url: URL) -> [String: String] {
        let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        return Dictionary(items.map { ($0.name, $0.value ?? "") }, uniquingKeysWith: { a, _ in a })
    }

    func testManualReportTargetsTheFormWithEnvironment() {
        let r = FeedbackReport.build(.init(title: "", environment: env))
        XCTAssertFalse(r.needsClipboard)
        XCTAssertTrue(r.url.absoluteString.hasPrefix("https://github.com/muni-works/vcad/issues/new?"))
        let q = query(r.url)
        XCTAssertEqual(q["template"], "bug_report.yml")
        XCTAssertEqual(q["environment"], "vcad 1.0.0 (42)\nmacOS 26.1.0\nchip: Apple M3 Pro\nworkspace: design")
        XCTAssertNil(q["logs"])
    }

    func testSpecialCharactersRoundTrip() {
        let r = FeedbackReport.build(.init(title: "a+b & c=d #1", environment: env, whatHappened: "x+y?&z"))
        let q = query(r.url)
        XCTAssertEqual(q["title"], "a+b & c=d #1")
        XCTAssertEqual(q["what-happened"], "x+y?&z")
    }

    func testLongDiagnosticsMoveToClipboard() {
        let big = String(repeating: "frame 0x1234 libfoo\n", count: 2_000)
        let r = FeedbackReport.build(.init(title: "Crash", environment: env, whatHappened: "short", diagnostics: big))
        XCTAssertTrue(r.needsClipboard)
        XCTAssertLessThanOrEqual(r.url.absoluteString.count, FeedbackReport.maxURLLength)
        XCTAssertEqual(query(r.url)["logs"], FeedbackReport.clipboardNote)
        XCTAssertEqual(query(r.url)["what-happened"], "short")
    }

    func testShortDiagnosticsStayInline() {
        let r = FeedbackReport.build(.init(title: "Crash", environment: env, diagnostics: "{\"a\":1}"))
        XCTAssertFalse(r.needsClipboard)
        XCTAssertEqual(query(r.url)["logs"], "{\"a\":1}")
    }

    func testOversizedSummaryIsTruncatedToFit() {
        let huge = String(repeating: "é long text ", count: 3_000)
        let r = FeedbackReport.build(.init(title: "T", environment: env, whatHappened: huge, diagnostics: "x"))
        XCTAssertLessThanOrEqual(r.url.absoluteString.count, FeedbackReport.maxURLLength)
        XCTAssertTrue(r.needsClipboard)
        XCTAssertTrue(query(r.url)["what-happened"]!.hasSuffix("…(truncated)"))
    }

    func testHomePathsAreScrubbed() {
        XCTAssertEqual(FeedbackReport.scrub("at /Users/alice/Documents/part.vcad"), "at ~/Documents/part.vcad")
        let r = FeedbackReport.build(.init(title: "", environment: env, whatHappened: "/Users/bob/x"))
        XCTAssertFalse(r.url.absoluteString.contains("bob"))
    }

    // MARK: diagnostic summaries

    let crashJSON = """
    {"crashDiagnostics":[{"diagnosticMetaData":{"appVersion":"1.0.0","appBuildVersion":"42",
    "osVersion":"macOS 26.1","exceptionType":1,"signal":11,"terminationReason":"Namespace SIGNAL, Code 11"},
    "callStackTree":{"callStacks":[
      {"threadAttributed":false,"callStackRootFrames":[{"binaryName":"other","offsetIntoBinaryTextSegment":1}]},
      {"threadAttributed":true,"callStackRootFrames":[{"binaryName":"vcad","offsetIntoBinaryTextSegment":255,
        "subFrames":[{"binaryName":"/Users/carol/lib/libvcad_ffi.dylib","address":4096}]}]}]}}],
    "hangDiagnostics":[{"diagnosticMetaData":{"hangDuration":"4.2 sec"},"callStackTree":{"callStacks":[]}}]}
    """

    func testCrashSummary() throws {
        let s = DiagnosticSummary.parse(payloadJSON: Data(crashJSON.utf8))
        XCTAssertEqual(s.count, 2)
        let c = s[0]
        XCTAssertEqual(c.kind, .crash)
        XCTAssertEqual(c.headline, "EXC_BAD_ACCESS (1) · SIGSEGV (11)")
        XCTAssertEqual(c.title, "Crash: EXC_BAD_ACCESS (1) · SIGSEGV (11)")
        XCTAssertEqual(c.frames, ["vcad +0xff", "~/lib/libvcad_ffi.dylib 0x1000"])
        XCTAssertEqual(c.text, """
        vcad quit unexpectedly.
        EXC_BAD_ACCESS (1) · SIGSEGV (11)
        app 1.0.0 (42)
        os macOS 26.1
        reason: Namespace SIGNAL, Code 11

        top frames:
        0  vcad +0xff
        1  ~/lib/libvcad_ffi.dylib 0x1000
        """)
    }

    func testHangSummary() {
        let h = DiagnosticSummary.parse(payloadJSON: Data(crashJSON.utf8))[1]
        XCTAssertEqual(h.kind, .hang)
        XCTAssertEqual(h.title, "Hang: hang 4.2 sec")
        XCTAssertTrue(h.frames.isEmpty)
    }

    func testFramesCapAndMalformedInput() {
        var frame: [String: Any] = ["binaryName": "leaf"]
        for i in 0..<40 { frame = ["binaryName": "f\(i)", "subFrames": [frame]] }
        let tree: [String: Any] = ["callStacks": [["callStackRootFrames": [frame]]]]
        XCTAssertEqual(DiagnosticSummary.topFrames(tree).count, DiagnosticSummary.maxFrames)
        XCTAssertTrue(DiagnosticSummary.parse(payloadJSON: Data("nope".utf8)).isEmpty)
        let bare = DiagnosticSummary.summarize([:], kind: .crash)
        XCTAssertEqual(bare.headline, "unknown exception")
    }
}
