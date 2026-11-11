import XCTest
import SwiftUI
import AppKit
@testable import VcadApp

/// The 3D print flow end to end against the engine's simulated printer
/// (`dry-run`): nothing here touches a network or a real machine.
@MainActor
final class PrintFlowTests: XCTestCase {
    /// A 20 × 15 × 10 block, the smallest real part the slicer accepts.
    private let block = Data("""
    {"version":"0.1","nodes":{"1":{"id":1,"name":"block","op":{"type":"Cube","size":{"x":20,"y":15,"z":10}}}},
     "materials":{},"part_materials":{},"roots":[{"root":1,"material":"default"}]}
    """.utf8)

    /// Never read the user's real printers.toml: a saved printer would get
    /// status polls from these tests. Only the simulated printer exists here.
    override func setUp() async throws {
        setenv("VCAD_PRINTERS", NSTemporaryDirectory() + "vcad-print-tests-none.toml", 1)
    }

    private func resetPrinter(_ extra: [String: Any] = [:]) async throws {
        var req: [String: Any] = ["op": "dry_run_reset", "step": 50.0]
        extra.forEach { req[$0] = $1 }
        _ = try await PrintStore.engine(nil, req)
    }

    private func log() async throws -> [String: Any] {
        try await PrintStore.engine(nil, ["op": "dry_run_log"])
    }

    private func readyStore() async throws -> PrintStore {
        try await resetPrinter()
        let s = PrintStore()
        s.includeDryRun = true
        await s.orient(doc: block)
        await s.sliceNow(doc: block, name: "block")
        await s.refreshPrinters()
        XCTAssertEqual(s.savedPrinters.map(\.name), ["dry-run"], "tests must only see the simulated printer")
        s.device = "dry-run"
        await s.refreshStatus()
        return s
    }

    // MARK: pure logic

    func testRepeatedOverhangWarningsCollapseToOneLine() {
        let w: [String: Any] = ["kind": "overhang", "message": "Face leans 180° from +Z — supports required", "face_indices": [3]]
        var list = Array(repeating: w, count: 8)
        list.append(["kind": "thin_wall", "message": "Wall 0.6 mm (min 0.8)", "face_indices": [9]])
        let g = WarningGroup.group(list)
        XCTAssertEqual(g.count, 2)
        XCTAssertEqual(g[0].message, "8 faces overhang and need support")
        XCTAssertEqual(g[0].faces.count, 8)
        XCTAssertEqual(g[1].message, "Wall 0.6 mm")
    }

    func testP1SProfileSurvivesLoadingTheList() {
        let s = PrintStore()
        s.loadPrinters()
        XCTAssertTrue(s.printers.contains { $0.id == "bambu_p1s" })
        XCTAssertEqual(s.printer, "bambu_p1s", "the default must not fall back to Generic (the gate refuses Generic)")
    }

    func testQualityPresetsDriveSettings() {
        let s = PrintStore()
        s.quality = .strong
        XCTAssertEqual(s.layerHeight, 0.16)
        XCTAssertEqual(s.walls, 5)
        XCTAssertEqual(s.pattern, "gyroid")
        s.quality = .fast
        XCTAssertEqual(s.settings["layer_height"] as? Double, 0.28)
    }

    func testChangingQualityInvalidatesTheSlice() {
        let s = PrintStore()
        s.prepared = ["printcheckOk": true, "lintOk": true]
        s.quality = .strong
        XCTAssertNil(s.prepared)
    }

    func testPrimaryActionFollowsTheStep() {
        let s = PrintStore()
        let modeled = PrintPose(id: 0, rotation: [[1, 0, 0], [0, 1, 0], [0, 0, 1]], supportFree: false, overhangCount: 8,
                                supportVolume: 5000, height: 40, printTime: 2900, grams: 15, name: "As modeled")
        let flipped = PrintPose(id: 1, rotation: [[1, 0, 0], [0, -1, 0], [0, 0, -1]], supportFree: true, overhangCount: 0,
                                supportVolume: 0, height: 40, printTime: 2400, grams: 13, name: "Flipped")
        s.poses = [modeled, flipped]
        XCTAssertEqual(s.primary, .orientToFix)
        s.fixByOrienting()
        XCTAssertEqual(s.pose, 1)
        XCTAssertEqual(s.primary, .next)
        s.step = .slice
        XCTAssertEqual(s.primary, .slice)
        s.step = .printer
        XCTAssertEqual(s.primary, .print)
        XCTAssertEqual(s.printBlocker, "Slice first.")
    }

    // MARK: engine + simulated printer

    func testOrientFindsASupportFreePose() async throws {
        let s = PrintStore()
        await s.orient(doc: block)
        XCTAssertNil(s.error)
        XCTAssertFalse(s.poses.isEmpty)
        XCTAssertTrue(s.selectedPose?.supportFree ?? false)
        XCTAssertNotNil(s.poses.first?.printTime)
    }

    func testSliceProducesACheckedJob() async throws {
        let s = PrintStore()
        await s.orient(doc: block)
        await s.sliceNow(doc: block, name: "block")
        XCTAssertNil(s.error)
        XCTAssertTrue(s.readyToPrint)
        XCTAssertNotNil(s.slice?["layers"])
    }

    func testPrinterReadyAndTrayPickedByMaterial() async throws {
        let s = try await readyStore()
        XCTAssertEqual(s.snapshot?.state, "idle")
        XCTAssertEqual(s.amsTray, 0, "PLA job should default to the PLA tray")
        XCTAssertNil(s.printBlocker)
    }

    func testWrongTrayBlocksPrint() async throws {
        let s = try await readyStore()
        s.amsTray = 1 // PETG
        XCTAssertEqual(s.printBlocker, "Slot 2 holds PETG, the job is PLA.")
    }

    func testUnconfirmedPrintNeverUploads() async throws {
        let s = try await readyStore()
        let ok = await s.startPrint(doc: block, name: "block", confirmed: false)
        XCTAssertTrue(ok, s.error ?? "")
        let l = try await log()
        XCTAssertEqual(l["uploaded"] as? Bool, false)
        XCTAssertEqual(l["printed"] as? Bool, false)
        XCTAssertNil(s.sent)
    }

    func testConfirmedPrintRunsPauseResumeToFinish() async throws {
        let s = try await readyStore()
        let ok = await s.startPrint(doc: block, name: "block", confirmed: true)
        XCTAssertTrue(ok, s.error ?? "")
        XCTAssertEqual(s.step, .print)
        XCTAssertEqual(s.snapshot?.state, "printing")
        XCTAssertEqual(s.primary, .pause)
        let l = try await log()
        XCTAssertEqual(l["ams"] as? [Int], [0])
        await s.control("pause")
        XCTAssertEqual(s.snapshot?.state, "paused")
        XCTAssertEqual(s.primary, .resume)
        await s.control("resume")
        XCTAssertEqual(s.snapshot?.state, "finished")
        XCTAssertEqual(s.primary, .again)
    }

    func testStopReturnsPrinterToIdle() async throws {
        try await resetPrinter(["step": 1.0])
        let s = PrintStore()
        s.includeDryRun = true
        await s.orient(doc: block)
        await s.sliceNow(doc: block, name: "block")
        await s.refreshPrinters()
        s.device = "dry-run"
        await s.refreshStatus()
        _ = await s.startPrint(doc: block, name: "block", confirmed: true)
        await s.control("stop")
        XCTAssertEqual(s.snapshot?.state, "idle")
    }

    func testBusyPrinterIsReportedNotSent() async throws {
        let s = try await readyStore()
        try await resetPrinter(["busy": true, "step": 0.0])
        await s.refreshStatus()
        XCTAssertEqual(s.printBlocker, "Printer is printing.")
        let ok = await s.startPrint(doc: block, name: "block", confirmed: true)
        XCTAssertFalse(ok)
        XCTAssertTrue(s.error?.contains("not idle") ?? false, s.error ?? "")
        let l = try await log()
        XCTAssertEqual(l["uploaded"] as? Bool, false)
    }

    // MARK: snapshots (opt-in)

    func testPrintPanelSnapshots() async throws {
        guard ProcessInfo.processInfo.environment["VCAD_CNC_SNAPSHOTS"] == "1" else { throw XCTSkip("Set VCAD_CNC_SNAPSHOTS=1 to render print panel snapshots.") }
        _ = NSApplication.shared
        let dir = URL(fileURLWithPath: ProcessInfo.processInfo.environment["VCAD_SNAPSHOT_DIR"] ?? "/tmp/vcad-print")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let model = EditorModel()
        let s = try await readyStore()
        // Show the "needs supports" state on Orient: pretend the part is modeled upside down.
        let modeled = PrintPose(id: 0, rotation: [[1, 0, 0], [0, 1, 0], [0, 0, 1]], supportFree: false, overhangCount: 8,
                                supportVolume: 5200, height: 45, printTime: 2966, grams: 15.9, name: "As modeled")
        let flipped = PrintPose(id: 1, rotation: [[1, 0, 0], [0, -1, 0], [0, 0, -1]], supportFree: true, overhangCount: 0,
                                supportVolume: 0, height: 45, printTime: 2460, grams: 13.2, name: "Flipped")
        let side = PrintPose(id: 2, rotation: [[0, 0, 1], [0, 1, 0], [-1, 0, 0]], supportFree: false, overhangCount: 2,
                             supportVolume: 900, height: 60, printTime: 3480, grams: 16.4, name: "On side")
        func shot(_ name: String) async throws {
            // The workbench without the glass surface: offscreen caching
            // renders Liquid Glass content monochrome.
            let content = VStack(alignment: .leading, spacing: 12) {
                PanelHeader(title: "3D Print")
                PrintWorkbench(model: model, store: FabStore.shared.print)
            }
            .controlSize(.small).frame(width: 280).padding(16)
            .background(Color(nsColor: .windowBackgroundColor))
            let hosting = NSHostingView(rootView: content)
            let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 320, height: 760), styleMask: [.titled], backing: .buffered, defer: false)
            window.appearance = NSAppearance(named: .darkAqua)
            window.contentView = hosting
            NSApp.setActivationPolicy(.regular); NSApp.activate()
            window.makeKeyAndOrderFront(nil)
            try? await Task.sleep(for: .milliseconds(300))
            hosting.layoutSubtreeIfNeeded()
            let bitmap = try XCTUnwrap(hosting.bitmapImageRepForCachingDisplay(in: hosting.bounds))
            hosting.cacheDisplay(in: hosting.bounds, to: bitmap)
            try XCTUnwrap(bitmap.representation(using: .png, properties: [:])).write(to: dir.appendingPathComponent("\(name).png"))
            window.orderOut(nil)
        }
        let fab = FabStore.shared
        fab.tab = .print
        // Swap the shared store's state for the prepared one.
        let p = fab.print
        p.includeDryRun = true
        p.poses = [modeled, flipped, side]; p.pose = 0
        p.step = .orient
        try await shot("1-orient-needs-support")
        p.pose = 1
        try await shot("2-orient-fixed")
        p.step = .slice
        p.prepared = s.prepared; p.slice = s.slice
        try await shot("3-slice")
        p.savedPrinters = s.savedPrinters; p.device = s.device; p.snapshot = s.snapshot; p.amsTray = s.amsTray
        p.step = .printer
        try await shot("4-printer")
        p.amsTray = 1
        try await shot("5-printer-wrong-slot")
        p.amsTray = 0
        p.savedPrinters = []
        p.discovered = [["model": "P1S", "name": "P1S-shop", "ip": "192.168.1.42", "serial": "01P00A000000000"]]
        try await shot("6-printer-pairing")
        p.savedPrinters = s.savedPrinters
        var snap = s.snapshot ?? PrinterSnapshot()
        snap.state = "printing"; snap.ready = false; snap.progress = 46; snap.layer = 23; snap.layers = 50
        snap.remainingMin = 22; snap.nozzle = 220; snap.bed = 55
        p.snapshot = snap; p.sent = ["sent": true]
        p.step = .print
        try await shot("7-printing")
    }
}
