import XCTest
@testable import VcadApp

@MainActor
final class ProgressCenterTests: XCTestCase {
    private var clock = Date(timeIntervalSince1970: 1_000)

    private func center() -> ProgressCenter {
        let c = ProgressCenter(interval: 0.1, now: { [unowned self] in self.clock })
        c.schedulesFlush = false
        return c
    }

    func testBeginAndEndPublishImmediately() {
        let c = center()
        c.begin("load", title: "Importing STEP", stage: "Reading file")
        XCTAssertEqual(c.tasks.map(\.id), ["load"])
        XCTAssertEqual(c.primary?.stage, "Reading file")
        c.end("load")
        XCTAssertTrue(c.tasks.isEmpty)
    }

    func testUpdatesAreThrottledToTheInterval() {
        let c = center()
        c.begin("load", title: "Importing STEP", stage: "Building faces", total: 100)
        // Same instant: held back.
        c.update("load", done: 10)
        c.update("load", done: 20)
        XCTAssertEqual(c.primary?.done, 0)
        clock += 0.05
        c.update("load", done: 30)
        XCTAssertEqual(c.primary?.done, 0, "still inside the 100 ms window")
        c.flush()
        XCTAssertEqual(c.primary?.done, 30, "the scheduled flush publishes the latest value")
        clock += 0.2
        c.update("load", done: 40)
        XCTAssertEqual(c.primary?.done, 40, "past the window: published at once")
    }

    func testStackHeadlinesNewestAndCancelIsUniform() {
        let c = center()
        var cancelled: [String] = []
        c.begin("load", title: "Opening", cancel: { cancelled.append("load") })
        c.begin("export", title: "Exporting STL", fraction: 0.4, cancel: { cancelled.append("export") })
        c.begin("solve", title: "Solving")
        XCTAssertEqual(c.primary?.id, "solve")
        XCTAssertFalse(c.primary!.cancellable)
        c.cancel("solve")                 // not cancellable: nothing happens
        XCTAssertEqual(c.tasks.count, 3)
        c.cancel("export")
        XCTAssertEqual(cancelled, ["export"])
        XCTAssertEqual(c.tasks.map(\.id), ["load", "solve"])
    }

    func testReportStartsThenUpdates() {
        let c = center()
        c.report("solve", title: "Solving", done: 1, total: 10)
        let started = c.primary!.started
        clock += 1
        c.report("solve", title: "Solving", done: 5, total: 10)
        XCTAssertEqual(c.primary?.done, 5)
        XCTAssertEqual(c.primary?.started, started, "report keeps the start time (ETA)")
    }

    func testLabelsUseCountsFractionAndEta() {
        let t0 = Date(timeIntervalSince1970: 0)
        var t = ProgressTask(id: "load", title: "Opening", stage: "Meshing parts",
                             done: 339, total: 1715, started: t0, cancellable: true)
        XCTAssertEqual(t.amountLabel, "339 of 1715")
        t.done = 100
        XCTAssertNil(t.etaLabel(now: t0.addingTimeInterval(10)), "under 10 % done: no guess")
        t.done = 500
        // 29 % in 20 s → ~49 s left.
        XCTAssertEqual(t.etaLabel(now: t0.addingTimeInterval(20)), "~49 s left")
        XCTAssertEqual(t.line, "Opening · Meshing parts · 29%")
        XCTAssertEqual(t.detailLine(now: t0.addingTimeInterval(20)), "500 of 1715 · ~49 s left")

        let f = ProgressTask(id: "x", title: "Importing STEP", stage: "Building faces",
                             explicitFraction: 0.127, started: t0, cancellable: false)
        XCTAssertEqual(f.amountLabel, "12%")
        XCTAssertEqual(f.line, "Importing STEP · Building faces · 12%")

        let none = ProgressTask(id: "y", title: "Solving", started: t0, cancellable: false)
        XCTAssertNil(none.fraction)
        XCTAssertEqual(none.line, "Solving")
        XCTAssertNil(none.detailLine(now: t0))
    }

    func testLoadProgressMapsToATask() {
        let url = URL(fileURLWithPath: "/tmp/Voron.step")
        var load = LoadProgress(url: url)
        XCTAssertEqual(load.taskTitle, "Importing STEP")
        XCTAssertEqual(load.taskStage, "Reading file")
        load.phase = .faces(done: 12, total: 100)
        XCTAssertEqual(load.taskStage, "Building faces")
        XCTAssertEqual(load.taskCounts.total, 100)
        load.phase = .evaluating(done: 339, total: 1715)
        XCTAssertEqual(load.taskTitle, "Opening")
        XCTAssertEqual(load.taskStage, "Meshing parts")
        XCTAssertNotNil(load.taskFraction)
        load.phase = .evaluating(done: 1715, total: 1715)
        XCTAssertEqual(load.taskStage, "Finishing", "all parts in, kernel still closing")
        XCTAssertNil(load.taskFraction, "indeterminate, not a full bar that never ends")
        XCTAssertEqual(load.taskCounts.total, 0)
    }
}
