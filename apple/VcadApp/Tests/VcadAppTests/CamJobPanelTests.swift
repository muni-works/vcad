import XCTest
@testable import VcadApp

/// CAM job panel: typed values commit without Tab, and a result never reads
/// as current once any input changed after it was generated.
@MainActor
final class CamJobPanelTests: XCTestCase {
    func testFieldParsesCompleteNumbersOnly() {
        XCTAssertEqual(FabField.parse("12.5"), 12.5)
        XCTAssertEqual(FabField.parse(" 7 "), 7)
        XCTAssertEqual(FabField.parse("-3"), -3)
        XCTAssertNil(FabField.parse(""))
        XCTAssertNil(FabField.parse("-"))
        XCTAssertNil(FabField.parse("abc"))
        XCTAssertEqual(FabField.format(3.175, 3), FabField.format(3.175, 3))
        XCTAssertEqual(FabField.parse(FabField.format(3.175, 3)), 3.175)
        XCTAssertEqual(FabField.parse(FabField.format(12000, 0)), 12000)
    }

    func testNoResultIsNeverStale() {
        XCTAssertFalse(CamJobStore().isStale)
    }

    func testResultWithoutRecordedInputsIsStale() {
        let s = CamJobStore()
        s.result = ["verification": ["pass": true, "graded": true]]
        XCTAssertTrue(s.isStale)
    }

    func testEveryInputKindChangesFingerprint() {
        let edits: [(String, (CamJobStore) -> Void)] = [
            ("stock", { $0.stockW += 1 }),
            ("clearance", { $0.clearance += 1 }),
            ("tool", { $0.tools[0].diameter = 4 }),
            ("tool added", { $0.tools.append(CamTool(number: 9)) }),
            ("op depth", { $0.ops[1].depth = 2 }),
            ("pocket X", { $0.ops[1].x = 5 }),
            ("op removed", { $0.ops.removeLast() }),
            ("post", { $0.post = "linuxcnc" }),
            ("verify toggle", { $0.verifyAgainstModel.toggle() }),
        ]
        for (name, edit) in edits {
            let s = CamJobStore()
            let before = s.inputsFingerprint
            edit(s)
            XCTAssertNotEqual(before, s.inputsFingerprint, "\(name) edit must change the fingerprint")
        }
    }

    func testFingerprintIsStableWithoutEdits() {
        let s = CamJobStore()
        XCTAssertEqual(s.inputsFingerprint, s.inputsFingerprint)
    }

    func testStaleVerdictNeverClaimsPassOrFail() {
        XCTAssertEqual(CamJobStore.verdict(pass: true, graded: true, stale: false), "Verified against the part")
        XCTAssertEqual(CamJobStore.verdict(pass: false, graded: true, stale: false), "Verification failed")
        XCTAssertFalse(CamJobStore.verdict(pass: true, graded: true, stale: true).contains("Verified"))
        XCTAssertFalse(CamJobStore.verdict(pass: false, graded: true, stale: true).contains("Verification failed"))
    }
}

@MainActor
final class DrawingStatusPillTests: XCTestCase {
    func testEmptyStateHidesPill() {
        XCTAssertFalse(DrawingStatusPill.hasContent(busy: false, hint: nil, error: "No part", hasResult: false))
        XCTAssertTrue(DrawingStatusPill.hasContent(busy: true, hint: nil, error: nil, hasResult: false))
        XCTAssertTrue(DrawingStatusPill.hasContent(busy: false, hint: "Click a view", error: nil, hasResult: false))
        XCTAssertTrue(DrawingStatusPill.hasContent(busy: false, hint: nil, error: nil, hasResult: true))
    }
}
