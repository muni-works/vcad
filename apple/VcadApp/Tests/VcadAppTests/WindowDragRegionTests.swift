import XCTest
@testable import VcadApp

final class WindowDragRegionTests: XCTestCase {
    func testTopRowMovesWindowViewportOrbits() {
        let h = Glass.toolbarHeight
        XCTAssertEqual(WindowDragRegion.owner(at: CGPoint(x: 400, y: 2), isWindowed: true), .windowDrag)
        XCTAssertEqual(WindowDragRegion.owner(at: CGPoint(x: 400, y: h - 0.5), isWindowed: true), .windowDrag)
        XCTAssertEqual(WindowDragRegion.owner(at: CGPoint(x: 400, y: h), isWindowed: true), .viewport)
        XCTAssertEqual(WindowDragRegion.owner(at: CGPoint(x: 400, y: 500), isWindowed: true), .viewport)
        XCTAssertEqual(WindowDragRegion.owner(at: CGPoint(x: 400, y: -1), isWindowed: true), .none)
    }

    func testRowCoversMoreThanTheSystemTitlebar() {
        // The row is taller than AppKit's ~28 pt title-bar strip, so that
        // strip alone can't own it.
        XCTAssertGreaterThan(Glass.toolbarHeight, 28)
        XCTAssertEqual(WindowDragRegion.owner(at: CGPoint(x: 10, y: 40), isWindowed: true), .windowDrag)
    }

    func testReleasedModeHasNoWindowDrag() {
        XCTAssertEqual(WindowDragRegion.owner(at: CGPoint(x: 400, y: 10), isWindowed: false), .viewport)
    }

    func testDoubleClickFollowsSystemSetting() {
        XCTAssertEqual(WindowDragRegion.doubleClickAction(action: "Maximize", legacyMiniaturize: nil), .zoom)
        XCTAssertEqual(WindowDragRegion.doubleClickAction(action: "Fill", legacyMiniaturize: nil), .zoom)
        XCTAssertEqual(WindowDragRegion.doubleClickAction(action: "Minimize", legacyMiniaturize: nil), .minimize)
        XCTAssertEqual(WindowDragRegion.doubleClickAction(action: "None", legacyMiniaturize: true), .none)
        XCTAssertEqual(WindowDragRegion.doubleClickAction(action: nil, legacyMiniaturize: true), .minimize)
        XCTAssertEqual(WindowDragRegion.doubleClickAction(action: nil, legacyMiniaturize: nil), .zoom)
    }

    @MainActor func testDragViewClaimsWindowMoves() {
        XCTAssertTrue(WindowDragNSView().mouseDownCanMoveWindow)
    }
}
