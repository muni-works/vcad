import SwiftUI
import CVcadFFI

// vcad on Vision Pro.
//
// The shared sources under Shared/ (symlinked from ../VcadApp) provide the
// editor: EditorModel, the document DAG, the kernel FFI bridge, the intent
// engine, the SwiftUI panels, and the platform-neutral half of release-to-
// desktop (ReleasedDesktop.swift). This target supplies what macOS can't
// share — the volumetric viewport (DocumentVolume), the released room
// (ReleasedSpace + ReleasedRoomWindows) and the spatial window layout.
//
// Two modes, the same as the Mac:
//  - windowed: one volumetric window; the part floats in it, and the Mac's
//    own chrome rides ornaments around it — GlassToolbar on top, the left and
//    right rails either side, the dock along the bottom;
//  - released: the toolbar's Release button lifts the bodies out into a mixed
//    ImmersiveSpace at real scale, and the same chrome floats beside them as
//    windows. The toolbar's Open in Window button puts the volume back.

@main
struct VcadVisionApp: App {
    @State private var model = EditorModel()
    @State private var intent = IntentEngine()
    @State private var immersion: ImmersionStyle = .mixed

    var body: some Scene {
        WindowGroup(id: ReleaseScene.volume) {
            DocumentVolume(model: model, intent: intent)
                .task {
                    let env = ProcessInfo.processInfo.environment
                    if let path = env["VCAD_OPEN"], !path.isEmpty, model.documentURL == nil {
                        model.openDocument(URL(fileURLWithPath: path))
                    }
                }
                .modifier(ReleaseLaunchHook(model: model))
        }
        .windowStyle(.volumetric)
        .defaultSize(width: 0.7, height: 0.55, depth: 0.55, in: .meters)

        // Released chrome: the Mac's toolbar + dock at hand height, its left
        // and right rails either side (shared views, ReleasedRoomWindows.swift).
        WindowGroup(id: ReleaseScene.palette) {
            ReleasedToolbarWindow(model: model)
        }
        .windowResizability(.contentSize)
        .defaultWindowPlacement { _, _ in WindowPlacement(.utilityPanel) }

        WindowGroup(id: ReleaseScene.navigator) {
            ReleasedNavigatorWindow(model: model)
        }
        .windowResizability(.contentSize)
        .defaultWindowPlacement { _, context in
            if let bar = context.windows.first(where: { $0.id == ReleaseScene.palette }) {
                return WindowPlacement(.leading(bar))
            }
            return WindowPlacement(.utilityPanel)
        }

        WindowGroup(id: ReleaseScene.inspector) {
            ReleasedInspectorWindow(model: model)
        }
        .windowResizability(.contentSize)
        .defaultWindowPlacement { _, context in
            if let bar = context.windows.first(where: { $0.id == ReleaseScene.palette }) {
                return WindowPlacement(.trailing(bar))
            }
            return WindowPlacement(.utilityPanel)
        }

        ImmersiveSpace(id: ReleaseScene.space) {
            ReleasedSpace(model: model)
        }
        .immersionStyle(selection: $immersion, in: .mixed)
    }
}
