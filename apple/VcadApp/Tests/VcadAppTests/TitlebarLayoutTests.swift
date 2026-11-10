import XCTest
import AppKit
@testable import VcadApp

/// The windowed document's titlebar: tall enough for the toolbar row natively
/// (no hand-resized container), traffic lights on the row's center, and the
/// row's empty space hit-testing to the drag region.
final class TitlebarLayoutTests: XCTestCase {
    @MainActor private func makeWindow() -> (KeyableWindow, WindowDragNSView) {
        let w = KeyableWindow(contentRect: NSRect(x: 100, y: 100, width: 1000, height: 700),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                              backing: .buffered, defer: false)
        w.titlebarAppearsTransparent = true
        w.titleVisibility = .hidden
        let content = NSView(frame: NSRect(x: 0, y: 0, width: 1000, height: 700))
        let drag = WindowDragNSView(frame: NSRect(x: 0, y: 700 - Glass.toolbarHeight,
                                                  width: 1000, height: Glass.toolbarHeight))
        content.addSubview(drag)
        w.contentView = content
        w.installTitlebarToolbar()
        w.layoutIfNeeded()
        w.alignTitlebarButtons()
        return (w, drag)
    }

    @MainActor func testTitlebarCoversTheRowWithoutManualResize() {
        let (w, _) = makeWindow()
        let close = w.standardWindowButton(.closeButton)!
        let container = close.superview!.superview!
        // AppKit sized it (unified toolbar), and it reaches the row's bottom.
        XCTAssertGreaterThanOrEqual(container.frame.height, Glass.toolbarHeight)
        XCTAssertEqual(w.toolbarStyle, .unified)
        w.alignTitlebarButtons()
        XCTAssertEqual(container.frame.height, close.superview!.superview!.frame.height,
                       "aligning must not resize AppKit's titlebar container")
    }

    @MainActor func testTrafficLightsCenterOnTheRow() {
        let (w, _) = makeWindow()
        for type in [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton] {
            let b = w.standardWindowButton(type)!
            let inWindow = b.convert(NSPoint(x: b.bounds.midX, y: b.bounds.midY), to: nil)
            XCTAssertEqual(w.frame.height - inWindow.y, Glass.toolbarCenter, accuracy: 1, "\(type)")
            // Fully inside the titlebar view, so not clipped and fully clickable.
            XCTAssertTrue(b.superview!.bounds.contains(b.frame), "\(type)")
        }
    }

    @MainActor func testEmptyRowHitTestsToDragRegion() {
        let (w, drag) = makeWindow()
        let frameView = w.contentView!.superview!
        for y in [4.0, 20, 40, Glass.toolbarHeight - 2] {
            XCTAssertTrue(frameView.hitTest(NSPoint(x: 400, y: w.frame.height - y)) === drag, "y=\(y)")
        }
    }
}
