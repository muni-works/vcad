import XCTest
@testable import VcadApp

/// Regressions found in live QA round 2 (docs/qa/live-qa-2026-11-11.md).
@MainActor
final class LiveQA2Tests: XCTestCase {
    /// Find (⌘K / header search) on the launch sandbox answered "No command
    /// matches" for Sketch, Extrude, Fillet: they were disabled until the
    /// sandbox became a document, and Find drops disabled commands.
    func testFindOffersModelingVerbsOnTheLaunchSandbox() throws {
        let m = EditorModel()
        XCTAssertTrue(m.source.isSandbox)
        XCTAssertFalse(m.usesDocumentTree)
        let enabled = Dictionary(m.paletteCommands.map { ($0.id, $0.enabled) }, uniquingKeysWith: { a, _ in a })
        for id in ["sketch", "op.extrude", "op.fillet", "measure", "params"] where enabled[id] != nil {
            XCTAssertEqual(enabled[id], true, "\(id) hidden from Find on the sandbox")
        }
        XCTAssertEqual(enabled["sketch"], true)

        // Running New Sketch promotes the sandbox and enters the sketch.
        let sketch = try XCTUnwrap(m.paletteCommands.first { $0.id == "sketch" })
        sketch.run()
        XCTAssertTrue(m.usesDocumentTree)
        XCTAssertTrue(m.sketching)
    }
}

@MainActor
final class LiveQA2PaletteDismissTests: XCTestCase {
    /// Find stuck open over a sketch: entering a sketch now closes it.
    func testEnteringSketchClosesFind() {
        let m = EditorModel()
        XCTAssertTrue(m.promoteSandboxToDocument(blank: true))
        m.showsCommandPalette = true
        m.enterSketch()
        XCTAssertTrue(m.sketching)
        XCTAssertFalse(m.showsCommandPalette)
    }
}

@MainActor
final class LiveQA2PaletteWorkspaceTests: XCTestCase {
    /// Find followed the user from Design into Drawing.
    func testSwitchingWorkspaceClosesFind() {
        let m = EditorModel()
        m.showsCommandPalette = true
        m.workspace = .drawing
        XCTAssertFalse(m.showsCommandPalette)
    }
}

final class LiveQA2PrintPoseTests: XCTestCase {
    private func name(_ down: [Double]) -> String? {
        PrintPose.parse(["rotation": [[1, 0, 0], [0, 1, 0], [0, 0, 1]], "down": down], index: 0)?.name
    }
    /// Orient listed three "On side" rows for the K1 PTO assembly.
    func testSidePosesNameTheRestingFace() {
        XCTAssertEqual(name([0, 0, -1]), "As modeled")
        XCTAssertEqual(name([0, 0, 1]), "Flipped")
        XCTAssertEqual(name([1, 0, 0]), "On right side")
        XCTAssertEqual(name([-1, 0, 0]), "On left side")
        XCTAssertEqual(name([0, 1, 0]), "On back")
        XCTAssertEqual(name([0, -1, 0]), "On front")
        XCTAssertEqual(name([0.6, 0, -0.8]), "Tilted")
    }
}

@MainActor
final class LiveQA2PrintFitTests: XCTestCase {
    /// The top-ranked pose overflowed the bed and was pre-selected, so Slice
    /// failed. A fitting pose must lead and be the default.
    func testPoseThatOverflowsTheBedIsNeverTheDefault() {
        let store = PrintStore()
        let id = [[1, 0, 0], [0, 1, 0], [0, 0, 1]]
        store.setPoses([
            ["rotation": id, "down": [1, 0, 0], "supportFree": false, "height": 60, "fits": false],
            ["rotation": id, "down": [0, -1, 0], "supportFree": false, "height": 60, "printTimeSeconds": 21000],
            ["rotation": id, "down": [0, 0, -1], "supportFree": true, "height": 240, "printTimeSeconds": 21600],
        ])
        XCTAssertEqual(store.poses.map(\.fits), [true, true, false])
        XCTAssertEqual(store.poses.map(\.id), [0, 1, 2])
        XCTAssertEqual(store.selectedPose?.name, "As modeled")
        XCTAssertTrue(store.selectedPose?.fits ?? false)
        XCTAssertEqual(store.bestPose, store.poses.firstIndex { $0.name == "As modeled" })
    }
}
