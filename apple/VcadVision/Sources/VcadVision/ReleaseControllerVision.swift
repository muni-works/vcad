import SwiftUI
import RealityKit
import UIKit

// The visionOS half of ReleaseWindowController. Its scene half (body dressing,
// hover/selection repaint, refresh) is shared from ReleasedDesktop.swift; the
// Mac half (NSWindow, event monitors, ARView projection) sits behind
// `#if os(macOS)` there. These are its visionOS answers: there is no 2D
// screen over the room, so screen projection answers "nowhere", and the window
// life cycle is SwiftUI scenes (ReleaseActions) rather than an NSWindow.
// Same member list as the iPhone port's stand-in (PhoneViewportController).

extension ReleaseWindowController {
    var hostWindow: UIWindow? { nil }
    var overlaySize: CGSize { .zero }

    func show(model: EditorModel, intent: IntentEngine) {}
    func hide() {}
    /// The toolbar's Release to Desktop / Open in Window button (shared
    /// GlassToolbar) lands here, as it does on the Mac: on Vision Pro it asks
    /// whichever scene is up to run the release / return (ReleaseActions).
    func setWindowed(_ windowed: Bool) { ReleasedRoom.shared.requestedWindowed = windowed }
    func updateDocumentWindow() {}
    func updateCNCWindowMinimum() {}
    func requestClose() {}
    func confirmClose() -> Bool { true }

    func viewPoint(forPart i: Int, from camera: SIMD3<Float>, lookAt: SIMD3<Float>) -> CGPoint? { nil }
    func viewPoint(forWorld w: SIMD3<Float>, from camera: SIMD3<Float>, lookAt: SIMD3<Float>) -> CGPoint? { nil }
    func projector(from entity: Entity, camera: SIMD3<Float>, lookAt: SIMD3<Float>) -> ((SIMD3<Float>) -> CGPoint?)? { nil }
    func hits(atViewPoint p: CGPoint) -> [CollisionCastHit] { [] }
}

/// What shared code reaches for through `ReleaseWindowController.arView`
/// (snapshots for history thumbnails and timelines, a projection for agent
/// pins). The room has no ARView, so `arView` is always nil and every one of
/// those paths takes its existing "no viewport" early return — this type only
/// gives them something to typecheck against.
final class RoomNoARView {
    var bounds: CGRect { .zero }
    var isFlipped: Bool { true }
    var window: UIWindow? { nil }
    func project(_ point: SIMD3<Float>) -> CGPoint? { nil }
    func snapshot(saveToHDR: Bool, completion: @escaping (UIImage?) -> Void) { completion(nil) }
}

extension ReleaseWindowController {
    var arView: RoomNoARView? { nil }
}
