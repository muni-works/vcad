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

    @MainActor func testDragViewIsNotAServerDragRegion() {
        // A server-side drag region swallowed the press in the real app.
        XCTAssertFalse(WindowDragNSView().mouseDownCanMoveWindow)
        XCTAssertTrue(WindowDragNSView().acceptsFirstMouse(for: nil))
    }

    @MainActor func testDragViewHitTestsItselfInTheEmptyRow() {
        // The row: a host view with the drag view behind a control.
        let row = NSView(frame: NSRect(x: 0, y: 0, width: 1000, height: Glass.toolbarHeight))
        let drag = WindowDragNSView(frame: row.bounds)
        row.addSubview(drag)
        let button = NSButton(frame: NSRect(x: 20, y: 10, width: 30, height: 30))
        row.addSubview(button)
        // Empty space between the sidebar toggle and the status field.
        XCTAssertTrue(row.hitTest(NSPoint(x: 200, y: 20)) === drag)
        XCTAssertTrue(drag.hitTest(NSPoint(x: 200, y: 20)) === drag)
        // Controls on top still win; outside the row nothing is claimed.
        XCTAssertTrue(row.hitTest(NSPoint(x: 30, y: 20)) === button)
        XCTAssertNil(drag.hitTest(NSPoint(x: 200, y: Glass.toolbarHeight + 5)))
    }
}
