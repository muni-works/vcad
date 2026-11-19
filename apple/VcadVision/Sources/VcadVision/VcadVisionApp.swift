import SwiftUI
import CVcadFFI

// vcad on Vision Pro.
//
// The shared sources under Shared/ (symlinked from ../VcadApp) provide the
// editor: EditorModel, the document DAG, the kernel FFI bridge, the intent
// engine, the SwiftUI panels, and the platform-neutral half of release-to-
// desktop (ReleasedDesktop.swift). This target supplies what macOS can't
// share — the volumetric viewport (DocumentVolume), the released room
// (ReleasedSpace + ReleasedPanels) and the spatial window layout.
//
// Two modes, the same as the Mac:
//  - windowed: one volumetric window; the part floats in it, panels ride
//    ornaments around it;
//  - released: "Release into Room" lifts the bodies out into a mixed
//    ImmersiveSpace at real scale, with the Component Palette and the Object
//    Inspector as floating windows beside them. "Return to Window" puts the
//    volume back.

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

        WindowGroup(id: ReleaseScene.palette) {
            ComponentPaletteWindow(model: model)
        }
        .windowResizability(.contentSize)
        // A tool palette belongs at hand height, in reach: the system's
        // utility-panel spot, below and in front of the released parts.
        .defaultWindowPlacement { _, _ in WindowPlacement(.utilityPanel) }

        WindowGroup(id: ReleaseScene.inspector) {
            ObjectInspectorPanel(model: model)
        }
        .windowResizability(.contentSize)
        .defaultWindowPlacement { _, context in
            // Beside the palette, the way the Mac floats the inspector next
            // to its tool windows (ReleaseActions opens the palette first).
            if let palette = context.windows.first(where: { $0.id == ReleaseScene.palette }) {
                return WindowPlacement(.trailing(palette))
            }
            return WindowPlacement(.utilityPanel)
        }

        ImmersiveSpace(id: ReleaseScene.space) {
            ReleasedSpace(model: model)
        }
        .immersionStyle(selection: $immersion, in: .mixed)
    }
}
