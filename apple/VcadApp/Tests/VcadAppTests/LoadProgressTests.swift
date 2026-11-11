import XCTest
@testable import VcadApp

/// The open-progress model behind the import / first-solve card.
final class LoadProgressTests: XCTestCase {
    private let step = URL(fileURLWithPath: "/tmp/411080037.STEP")
    private let doc = URL(fileURLWithPath: "/tmp/bracket.vcad")

    func testKindFromExtension() {
        XCTAssertEqual(LoadProgress.Kind(url: step), .step)
        XCTAssertEqual(LoadProgress.Kind(url: URL(fileURLWithPath: "/a/b.stp")), .step)
        XCTAssertEqual(LoadProgress.Kind(url: URL(fileURLWithPath: "/a/b.loon")), .loon)
        XCTAssertEqual(LoadProgress.Kind(url: URL(fileURLWithPath: "/a/r.urdf")), .urdf)
        XCTAssertEqual(LoadProgress.Kind(url: doc), .vcad)
    }

    func testStepStartsImportingAndDocumentStartsEvaluating() {
        let s = LoadProgress(url: step)
        XCTAssertEqual(s.phase, .reading)
        XCTAssertTrue(s.isImporting && s.isRunning)
        let d = LoadProgress(url: doc)
        XCTAssertEqual(d.phase, .evaluating(done: 0, total: 0))
        XCTAssertFalse(d.isImporting)
        XCTAssertTrue(d.isRunning)
    }

    func testImportStageCodes() {
        XCTAssertEqual(LoadProgress.importPhase(stage: 0, done: 0, total: 0), .reading)
        XCTAssertEqual(LoadProgress.importPhase(stage: 1, done: 0, total: 0), .parsing)
        XCTAssertEqual(LoadProgress.importPhase(stage: 2, done: 3, total: 9), .faces(done: 3, total: 9))
        XCTAssertEqual(LoadProgress.importPhase(stage: 3, done: 9, total: 9), .faces(done: 9, total: 9))
    }

    func testStepFractionSpansImportThenMeshingMonotonically() {
        var s = LoadProgress(url: step)
        XCTAssertNil(s.fraction, "reading has nothing to measure")
        s.phase = .parsing
        XCTAssertNil(s.fraction)
        var last = -1.0
        for d in 0...10 {
            s.phase = .faces(done: d, total: 10)
            let f = try! XCTUnwrap(s.fraction)
            XCTAssertGreaterThanOrEqual(f, last); last = f
        }
        XCTAssertEqual(last, LoadProgress.importShare, accuracy: 1e-9)
        for d in 0...4 {
            s.phase = .evaluating(done: d, total: 4)
            let f = try! XCTUnwrap(s.fraction)
            XCTAssertGreaterThanOrEqual(f, last); last = f
        }
        XCTAssertEqual(last, 1, accuracy: 1e-9)
    }

    func testFractionIsClampedAndIndeterminateWithoutSteps() {
        var d = LoadProgress(url: doc)
        XCTAssertNil(d.fraction, "0 of 0: the kernel hasn't counted yet")
        d.phase = .evaluating(done: 0, total: 1)
        XCTAssertNil(d.fraction, "a single-part solve has no steps")
        d.phase = .evaluating(done: 7, total: 4)
        XCTAssertEqual(d.fraction, 1)
        d.phase = .evaluating(done: 1, total: 4)
        XCTAssertEqual(d.fraction!, 0.25, accuracy: 1e-9)
        var s = LoadProgress(url: step)
        s.phase = .faces(done: 0, total: 0)
        XCTAssertNil(s.fraction)
    }

    func testStageLabels() {
        var s = LoadProgress(url: step)
        XCTAssertEqual(s.stageLabel, "Reading file")
        s.phase = .faces(done: 120, total: 480)
        XCTAssertEqual(s.stageLabel, "Building faces 120 of 480")
        s.phase = .evaluating(done: 2, total: 5)
        XCTAssertEqual(s.stageLabel, "Meshing parts 2 of 5")
        s.phase = .evaluating(done: 0, total: 1)
        XCTAssertEqual(s.stageLabel, "Meshing")
        var d = LoadProgress(url: doc)
        d.phase = .evaluating(done: 3, total: 8)
        XCTAssertEqual(d.stageLabel, "Evaluating parts 3 of 8")
    }

    func testCardFoldsToPillOnceGeometryLandsButErrorsStayCards() {
        var d = LoadProgress(url: doc)
        XCTAssertEqual(d.presentation, .card)
        d.landedGeometry = true
        XCTAssertEqual(d.presentation, .pill)
        d.phase = .failed(message: "x", details: nil)
        XCTAssertEqual(d.presentation, .card)
        XCTAssertFalse(d.isRunning)
        d.phase = .warning(message: "w", details: "report")
        XCTAssertEqual(d.presentation, .card)
        XCTAssertEqual(d.details, "report")
    }

    func testElapsedAppearsAfterTwoSecondsWhileRunning() {
        let t0 = Date(timeIntervalSince1970: 1000)
        var d = LoadProgress(url: doc, started: t0)
        XCTAssertNil(d.elapsedLabel(now: t0.addingTimeInterval(1.5)))
        XCTAssertEqual(d.elapsedLabel(now: t0.addingTimeInterval(12.4)), "12 s")
        XCTAssertEqual(d.elapsedLabel(now: t0.addingTimeInterval(64)), "1:04")
        d.phase = .failed(message: "x", details: nil)
        XCTAssertNil(d.elapsedLabel(now: t0.addingTimeInterval(30)))
    }

    func testFailureShortensLongMessagesAndKeepsDetails() {
        let long = String(repeating: "a", count: 200) + "\nsecond line"
        guard case let .failed(msg, details) = LoadProgress.failure(long, fallback: "f") else { return XCTFail() }
        XCTAssertLessThanOrEqual(msg.count, 90)
        XCTAssertEqual(details, long)
        XCTAssertEqual(LoadProgress.failure(nil, fallback: "Import failed"),
                       .failed(message: "Import failed", details: nil))
    }

    func testImportWarningOnlyWhenFacesSkipped() {
        XCTAssertNil(LoadProgress.importWarning(skippedFaces: 0, report: nil))
        guard case let .warning(msg, details) = LoadProgress.importWarning(skippedFaces: 3, report: "solid #1: …")
        else { return XCTFail() }
        XCTAssertTrue(msg.contains("3 faces"))
        XCTAssertEqual(details, "solid #1: …")
    }

    /// Headless (synchronous) opens never show the card: it belongs to the
    /// live window, and a test or smoke hook must not wait on it.
    @MainActor
    func testHeadlessOpenShowsNoCard() throws {
        let m = EditorModel()
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("loadprog-\(UUID())")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let url = dir.appendingPathComponent("p.vcad")
        try Data(#"{"version":"0.1","nodes":{},"materials":{},"part_materials":{},"roots":[]}"#.utf8).write(to: url)
        m.openDocument(url)
        XCTAssertNil(m.loading)
    }

    /// Cancel during a first solve goes back to what was open before.
    @MainActor
    func testCancelFirstSolveRestoresPreviousSource() {
        let m = EditorModel()
        m.loadingPrevious = .sandbox
        m.loading = LoadProgress(url: doc)
        m.cancelLoading()
        XCTAssertNil(m.loading)
        XCTAssertNil(m.loadingPrevious)
        XCTAssertTrue(m.source.isSandbox)
    }
}
