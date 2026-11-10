import SwiftUI
#if os(macOS)
import AppKit
#endif

// The window's top row is a title bar; the viewport below it is a camera.
// They used to overlap: the viewport's clear gesture layer runs full-bleed
// under the toolbar, and the toolbar's empty space (around the status field
// and the panel toggles) isn't hit-testable in SwiftUI, so a drag there fell
// through to the orbit gesture. In the strip AppKit also treats as the
// transparent title bar, the same drag moved the window too — both at once.
//
// The fix gives the row an explicit owner: a drag-region NSView behind the
// toolbar's controls. It is hit-testable, so the gesture layer below never
// sees a press that starts in the row; and it moves the window itself with
// `performDrag(with:)`, so the move works across the whole row, not just the
// ~28 pt AppKit considers title bar. Presses in the viewport never reach it.

/// Pure geometry + policy for the title-bar drag region, kept free of AppKit
/// event types so it is unit-testable.
enum WindowDragRegion {
    /// Which control a point in the overlay (top-left origin) belongs to.
    enum Owner: Equatable { case windowDrag, viewport, none }

    /// A press at `point` — in a windowed document, the toolbar row (above
    /// `Glass.toolbarHeight`) moves the window; everything below is the
    /// viewport. Released to the desktop there is no window to move: the
    /// row is a glass capsule and only the viewport claims anything.
    static func owner(at point: CGPoint, isWindowed: Bool,
                      toolbarHeight: CGFloat = Glass.toolbarHeight) -> Owner {
        if point.y < 0 { return .none }
        if isWindowed && point.y < toolbarHeight { return .windowDrag }
        return .viewport
    }

    /// What a double-click in the title bar does, per System Settings ›
    /// Desktop & Dock › "Double-click a window's title bar to".
    enum DoubleClickAction: Equatable { case zoom, minimize, none }

    /// Reads `AppleActionOnDoubleClick` ("Maximize"/"Fill" → zoom,
    /// "Minimize", "None"), falling back to the legacy
    /// `AppleMiniaturizeOnDoubleClick` bool, defaulting to zoom.
    static func doubleClickAction(action: String?, legacyMiniaturize: Bool?) -> DoubleClickAction {
        switch action?.lowercased() {
        case "minimize": return .minimize
        case "none": return .none
        case "maximize", "fill", "zoom": return .zoom
        default: return legacyMiniaturize == true ? .minimize : .zoom
        }
    }

    static func doubleClickAction(defaults: UserDefaults = .standard) -> DoubleClickAction {
        let legacy = defaults.object(forKey: "AppleMiniaturizeOnDoubleClick") as? Bool
        return doubleClickAction(action: defaults.string(forKey: "AppleActionOnDoubleClick"),
                                 legacyMiniaturize: legacy)
    }
}

#if os(macOS)
/// The AppKit view that owns presses in the toolbar row's empty space.
final class WindowDragNSView: NSView {
    override var mouseDownCanMoveWindow: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func mouseDown(with event: NSEvent) {
        guard let window else { return }
        // Released to the desktop the window is borderless: nothing to move.
        guard window.styleMask.contains(.titled) else { return }
        if event.clickCount == 2 {
            switch WindowDragRegion.doubleClickAction() {
            case .zoom: window.zoom(nil)
            case .minimize: window.miniaturize(nil)
            case .none: break
            }
            return
        }
        window.performDrag(with: event)
    }
}

/// Place behind the toolbar row's controls.
struct WindowDragArea: NSViewRepresentable {
    func makeNSView(context: Context) -> WindowDragNSView { WindowDragNSView() }
    func updateNSView(_ nsView: WindowDragNSView, context: Context) {}
}
#endif
