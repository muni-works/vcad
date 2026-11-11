import AppKit
import RealityKit

/// Render on demand: an idle viewport draws no frames.
///
/// RealityKit's macOS `ARView` renders at display rate forever off its own
/// CVDisplayLink, and because the release window is a transparent full-screen
/// layer, every one of those frames is also a full-screen WindowServer
/// composite (an idle window measured ~70% WindowServer CPU).
///
/// Anything that can change the picture calls `wake()`: input in the app, a
/// SwiftUI update of the viewport (camera, geometry, selection, hover, and the
/// timers that drive playback or spin all land there), a resize, a snapshot.
/// The view keeps rendering for a short grace period after the last wake so
/// in-flight work (async geometry, highlight repaints) gets its frames, then
/// stops the RealityKit engine; the last frame stays on the layer.
///
/// There is no public pause. `ARView.__preferredFrameRate` /
/// `__enableAutomaticFrameRate` were tried and change nothing (the engine
/// still ticks ~60–85 Hz); calling the view's own resign-active handler is a
/// no-op too. `__Engine.__stop()` / `__start()` on the shared engine is what
/// actually halts the display-link clock. The app has exactly one ARView, so
/// stopping the shared engine stops nothing else.
@MainActor
enum RenderGate {
    /// Seconds to keep rendering after the last wake.
    static var grace: TimeInterval = 0.75
    private(set) static var paused = false
    private static var deadline: CFAbsoluteTime = 0
    private static var timer: Timer?
    private static var installed = false
    /// Frames counted while `VCAD_FRAME_COUNT=1` (debug aid for idle tests).
    nonisolated(unsafe) static var frames = 0

    /// `VCAD_RENDER_GATE=0` restores always-on rendering (A/B, escape hatch).
    static let enabled = ProcessInfo.processInfo.environment["VCAD_RENDER_GATE"] != "0"
    private static var engine: __Engine? { __ServiceLocator.__sharedEngine }

    /// Start gating `ar`. Idempotent.
    static func install(on ar: ARView) {
        guard !installed else { return }
        installed = true
        // Input anywhere in the app may move the camera, hover a part or
        // drive a gizmo; waking on all of it is cheap and never misses one.
        let mask: NSEvent.EventTypeMask = [.mouseMoved, .leftMouseDown, .leftMouseUp, .leftMouseDragged,
                                           .rightMouseDown, .rightMouseUp, .rightMouseDragged,
                                           .otherMouseDown, .otherMouseUp, .otherMouseDragged,
                                           .scrollWheel, .magnify, .rotate, .keyDown, .keyUp, .flagsChanged]
        _ = NSEvent.addLocalMonitorForEvents(matching: mask) { e in
            MainActor.assumeIsolated { wake() }
            return e
        }
        if ProcessInfo.processInfo.environment["VCAD_FRAME_COUNT"] == "1" {
            frameCounter = ar.scene.subscribe(to: SceneEvents.Update.self) { _ in frames += 1 }
            let t = Timer(timeInterval: 5, repeats: true) { _ in
                MainActor.assumeIsolated {
                    log("[FRAMES] \(frames) paused=\(paused)")
                    frames = 0
                }
            }
            RunLoop.main.add(t, forMode: .common)
        }
        wake()
    }
    nonisolated(unsafe) private static var frameCounter: Any?

    private static func log(_ s: String) {
        guard ProcessInfo.processInfo.environment["VCAD_FRAME_COUNT"] == "1" else { return }
        let path = "/tmp/vcad_frames.log"
        if !FileManager.default.fileExists(atPath: path) {
            FileManager.default.createFile(atPath: path, contents: nil)
        }
        if let fh = FileHandle(forWritingAtPath: path) {
            fh.seekToEndOfFile(); fh.write(Data((s + "\n").utf8)); try? fh.close()
        }
    }

    /// Something may change the picture: render for at least `grace` seconds.
    static func wake(for seconds: TimeInterval? = nil) {
        deadline = max(deadline, CFAbsoluteTimeGetCurrent() + (seconds ?? grace))
        if paused {
            engine?.__start()
            paused = false
        }
        guard installed, timer == nil else { return }
        let t = Timer(timeInterval: 0.1, repeats: true) { _ in
            MainActor.assumeIsolated { check() }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    private static func check() {
        guard CFAbsoluteTimeGetCurrent() >= deadline else { return }
        timer?.invalidate(); timer = nil
        guard enabled, !paused, let e = engine else { return }
        e.__stop()
        paused = true
    }
}

/// The viewport's ARView: a resize must repaint at the new size.
final class GatedARView: ARView {
    override func layout() {
        super.layout()
        RenderGate.wake()
    }
}
