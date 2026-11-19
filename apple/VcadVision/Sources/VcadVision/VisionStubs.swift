import Foundation
import SwiftUI

// Stand-ins for Mac-only subsystems the shared EditorModel holds a handle to.
// Each is a deliberate no-op (or a refusal) on Vision Pro — listed in the
// ported-vs-stubbed note of the visionOS release PR. They keep the shared
// model word-for-word instead of sprinkling `#if os(macOS)` through it.

/// The agent edit path (AgentEdits.swift) rides the Mac's agent turn, cursor
/// and chrome. The one shared caller on visionOS is the DFM panel's one-click
/// fix, which refuses here (stubbed) instead of editing outside an undo step.
enum AgentEdits {
    struct Failure: Error { let message: String }
    @discardableResult
    static func edit(_ m: EditorModel, name: String, standalone: Bool = false,
                     _ body: (DocumentEngine) throws -> DocumentEngine.Result) throws -> DocumentEngine.Result {
        throw Failure(message: "\(name) isn't available on Vision Pro yet — apply it from the Mac app.")
    }
    static func describe(_ error: Error) -> String {
        if let r = error as? DocumentEngine.Refusal { return "\(r.kind): \(r.message)" }
        if let f = error as? Failure { return f.message }
        return String(describing: error)
    }
}

/// Render-on-demand gate (RenderGate.swift) drives the Mac's ARView draw
/// loop. RealityView on visionOS renders continuously, so waking is a no-op.
enum RenderGate {
    static func wake() {}
}


/// Path-traced viewport (PathTracer.swift: irys + IOSurface): Mac-only.
@MainActor final class PathTracer {}

/// Agent viewport labels (AgentBridge.swift): the bridge is Mac-only.
struct AgentAnnotation: Identifiable, Equatable {
    let id: String
    let instance: Int
    var text: String
}

/// Live loon hot-reload (LoonLoop.swift) watches the program's directory
/// with FSEvents, which visionOS doesn't have: no watcher here. Opening a
/// `.loon` still compiles it (DocumentBytes.swift).
@MainActor final class LoonWatcher {}




/// Launch/solve timing log (LaunchPerf.swift, `VCAD_PERF=1`) drives an ARView
/// orbit benchmark on the Mac. Off on visionOS.
enum LaunchPerf {
    static let enabled = false
    static var sinceLaunchMs: Double { 0 }
    static func once(_ label: String, _ extra: String = "") {}
    static func write(_ line: String) {}
}

/// The Mac shell's chrome state (GlassChrome.swift). On visionOS the editor's
/// status flashes go nowhere yet — the room has no status field.
@MainActor final class GlassUI {
    static let shared = GlassUI()
    var shortcutsShown = false
    var panel: AnyHashable?
    func flash(_ text: String, symbol: String) {}
}

/// Animated camera glides (NavigationViews.swift). There's no camera in a
/// volume — the person is the camera — so a glide just lands.
@MainActor final class NavigationUI {
    static let shared = NavigationUI()
    func glide(_ model: EditorModel, to pose: CameraPose, duration: Double = 0.42) {
        model.cameraPose = pose
    }
}

extension EditorModel {
    var cameraPose: CameraPose {
        get { CameraPose(azimuth: azimuth, elevation: elevation, distance: distance, pan: panOffset) }
        set {
            azimuth = newValue.azimuth; elevation = newValue.elevation
            distance = newValue.distance; panOffset = newValue.pan
            pinchBaseline = newValue.distance
        }
    }

    // Untitled-work autosave + Recents (DataTrust.swift) use AppKit alerts
    // and save panels; on visionOS saves just save.
    func noteEditForAutosave() {}
    func didSaveDocument(_ data: Data, at url: URL) { saveError = nil }
}

/// Renderer settings form (RendererSettings.swift needs the irys headers).
struct RendererSettingsForm: View {
    var body: some View { Text("Photoreal settings are on the Mac.").foregroundStyle(.secondary) }
}

/// irys (the in-process path tracer) is a Mac dylib; on visionOS it's never
/// loaded, so shared code takes its "linked photoreal renderer" fallback.
enum IrysLib {
    struct Unavailable: Error {}
    static let shared: Result<Void, Error> = .failure(Unavailable())
}

extension EditorModel {
    // Loon hot reload (LoonLoop.swift) has no FSEvents watch on visionOS.
    func watchLoonSources() {}
    func rebuildLoon() {}
    // Live path-traced viewport (PathTracer.swift): Mac-only.
    func setPathTrace(_ on: Bool) {}
    func requestPathTracedStill() {}
    // The photoreal still (PoseStudio.swift) is macOS-only there too.
    func photorealSnapshot(spp: Int = 96, completion: (@MainActor @Sendable (_ path: String?, _ error: String?) -> Void)? = nil) {
        completion?(nil, "Photoreal stills render on the Mac.")
    }
}
