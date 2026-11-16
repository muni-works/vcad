import Foundation
import simd

/// How mouse and trackpad input drives the viewport camera.
///
/// The mapping is pure data — the viewport asks `NavigationScheme.action`
/// what an input means and then does it — so every binding is unit-tested
/// without a window.
enum NavigationScheme: String, CaseIterable, Identifiable, Sendable {
    /// Fusion 360's bindings (the default): left-drag selects, the middle
    /// button moves the camera, the wheel zooms about the pointer.
    case fusion360 = "fusion360"
    /// SolidWorks: the middle button orbits, ⌃-middle pans.
    case solidworks = "solidworks"
    /// Onshape: right-drag orbits, middle-drag (or ⌃-right-drag) pans. A
    /// right-click that doesn't move still opens the context menu.
    case onshape = "onshape"
    /// Blender: the middle button orbits, ⇧-middle pans, the wheel zooms
    /// toward the screen centre.
    case blender = "blender"
    /// vcad's original bindings: left-drag orbits, ⇧-left-drag pans.
    case classic = "classic"

    var id: String { rawValue }

    var label: String {
        switch self {
        case .fusion360: "Fusion 360"
        case .solidworks: "SolidWorks"
        case .onshape: "Onshape"
        case .blender: "Blender"
        case .classic: "vcad classic"
        }
    }

    static let `default`: NavigationScheme = .fusion360
    static let prefsKey = "vcad.prefs.navigation"

    /// The scheme the user picked in Settings (Fusion 360 when unset).
    static var current: NavigationScheme {
        UserDefaults.standard.string(forKey: prefsKey).flatMap(NavigationScheme.init(rawValue:)) ?? .default
    }

    /// The physical inputs the viewport distinguishes.
    enum Input: Equatable, Sendable {
        enum Button: Equatable, Sendable { case left, middle, right }
        /// A drag with a mouse button held. `onHandle` = it started on a
        /// gizmo handle; `onEmpty` = it started on empty space (no part).
        case drag(Button, shift: Bool, command: Bool, control: Bool = false,
                  onHandle: Bool = false, onEmpty: Bool = true)
        /// A scroll: `precise` = trackpad / Magic Mouse (two-finger swipe),
        /// otherwise a notched mouse wheel.
        case scroll(precise: Bool, shift: Bool)
        /// Trackpad pinch.
        case pinch
        /// A double click.
        case doubleClick(Button)
    }

    /// What the viewport does with an input.
    enum Action: Equatable, Sendable {
        case orbit, pan
        /// Dolly toward the orbit target (the screen centre).
        case zoom
        /// Dolly toward whatever is under the pointer.
        case zoomAtCursor
        /// Rubber-band selection.
        case marquee
        /// Drag the gizmo handle under the pointer.
        case dragHandle
        /// Frame the part under the pointer (left double-click).
        case framePart
        /// Fit everything in view.
        case fitAll
        /// Not a camera input: let the event through (e.g. right-click menus).
        case none
    }

    /// What `input` means under this scheme. `sketching` disables the
    /// selection rubber band (a sketch picks its own entities with clicks).
    func action(for input: Input, sketching: Bool = false) -> Action {
        switch (self, input) {
        // A drag that starts on a gizmo handle is a transform in every scheme.
        case (_, .drag(.left, _, _, _, true, _)): return .dragHandle
        // Onshape is the one scheme that moves the camera with the right button.
        case (.onshape, .drag(.right, _, _, let control, _, _)): return control ? .pan : .orbit
        // Everywhere else right-drag is the marking menu / context menu.
        case (_, .drag(.right, _, _, _, _, _)): return .none
        case (_, .pinch): return .zoom
        case (_, .doubleClick(.left)): return sketching ? .none : .framePart
        case (_, .doubleClick(.middle)): return .fitAll
        case (_, .doubleClick(.right)): return .none

        // Fusion 360.
        case (.fusion360, .drag(.middle, let shift, _, _, _, _)): return shift ? .orbit : .pan

        // SolidWorks.
        case (.solidworks, .drag(.middle, _, _, let control, _, _)): return control ? .pan : .orbit

        // Onshape: middle-drag pans (the right button orbits, above).
        case (.onshape, .drag(.middle, _, _, _, _, _)): return .pan

        // Blender.
        case (.blender, .drag(.middle, let shift, _, _, _, _)): return shift ? .pan : .orbit
        case (.blender, .scroll(false, _)): return .zoom

        // Fusion, SolidWorks, Onshape and Blender share the rest: left-drag
        // box-selects, the wheel zooms at the pointer, and the trackpad keeps
        // the same two-finger pan / ⇧ orbit so switching presets for a mouse
        // never changes how the trackpad feels.
        case (.fusion360, .drag(.left, _, _, _, _, _)), (.solidworks, .drag(.left, _, _, _, _, _)),
             (.onshape, .drag(.left, _, _, _, _, _)), (.blender, .drag(.left, _, _, _, _, _)):
            return sketching ? .none : .marquee
        case (.fusion360, .scroll(true, let shift)), (.solidworks, .scroll(true, let shift)),
             (.onshape, .scroll(true, let shift)), (.blender, .scroll(true, let shift)):
            return shift ? .orbit : .pan
        case (.fusion360, .scroll(false, _)), (.solidworks, .scroll(false, _)),
             (.onshape, .scroll(false, _)):
            return .zoomAtCursor

        // vcad classic.
        case (.classic, .drag(.middle, let shift, _, _, _, _)): return shift ? .orbit : .pan
        case (.classic, .drag(.left, let shift, let command, _, _, let onEmpty)):
            if command && onEmpty && !sketching { return .marquee }
            return shift ? .pan : .orbit
        case (.classic, .scroll(_, _)): return .zoom
        }
    }

    /// The new orbit target for a zoom about the pointer. `rayDirection` is
    /// the world ray through the pointer from `camera`; the pivot is where it
    /// crosses the plane through the current target facing the camera. Both
    /// camera and target scale about that pivot by `newDistance / distance`,
    /// so the pivot stays put on screen.
    static func zoomTarget(panOffset: SIMD3<Float>, camera: SIMD3<Float>, rayDirection: SIMD3<Float>,
                           distance: Float, newDistance: Float) -> SIMD3<Float> {
        let fwd = simd_normalize(panOffset - camera)
        let denom = simd_dot(rayDirection, fwd)
        guard distance > 1e-9, denom > 1e-4 else { return panOffset }
        let t = simd_dot(panOffset - camera, fwd) / denom
        let pivot = camera + rayDirection * t
        return pivot + (panOffset - pivot) * (newDistance / distance)
    }

    /// One-line summaries for Settings and help, in display order.
    var summary: [(gesture: String, action: String)] {
        switch self {
        case .fusion360:
            return [("Middle-drag", "Pan"), ("⇧ Middle-drag", "Orbit"),
                    ("Scroll wheel", "Zoom at pointer"), ("Double-click middle", "Fit all"),
                    ("Two-finger swipe", "Pan"), ("⇧ Two-finger swipe", "Orbit"),
                    ("Pinch", "Zoom"), ("Left-drag", "Box select")]
        case .solidworks:
            return [("Middle-drag", "Orbit"), ("⌃ Middle-drag", "Pan"),
                    ("Scroll wheel", "Zoom at pointer"), ("Double-click middle", "Fit all"),
                    ("Two-finger swipe", "Pan"), ("⇧ Two-finger swipe", "Orbit"),
                    ("Pinch", "Zoom"), ("Left-drag", "Box select")]
        case .onshape:
            return [("Right-drag", "Orbit"), ("Middle-drag", "Pan"), ("⌃ Right-drag", "Pan"),
                    ("Scroll wheel", "Zoom at pointer"), ("Double-click middle", "Fit all"),
                    ("Two-finger swipe", "Pan"), ("⇧ Two-finger swipe", "Orbit"),
                    ("Pinch", "Zoom"), ("Left-drag", "Box select")]
        case .blender:
            return [("Middle-drag", "Orbit"), ("⇧ Middle-drag", "Pan"),
                    ("Scroll wheel", "Zoom"), ("Double-click middle", "Fit all"),
                    ("Two-finger swipe", "Pan"), ("⇧ Two-finger swipe", "Orbit"),
                    ("Pinch", "Zoom"), ("Left-drag", "Box select")]
        case .classic:
            return [("Drag", "Orbit"), ("⇧ Drag", "Pan"), ("⌘ Drag", "Box select"),
                    ("Middle-drag", "Pan"), ("Scroll / pinch", "Zoom")]
        }
    }
}
