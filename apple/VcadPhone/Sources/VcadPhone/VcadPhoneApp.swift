import SwiftUI
import CVcadFFI

// vcad on iPhone — the full editor.
//
// The shared sources (App/, the whole Mac source tree) provide everything
// below the chrome: EditorModel, the document DAG + runtime, the kernel FFI
// bridge, undo, the intent engine, sims, and the SwiftUI panels (feature
// tree, parameter editors, inspector, composer, playback). This target adds
// what a phone needs instead of a Mac window: the touch viewport
// (PhoneViewport), a phone layout (PhoneRootView), and Files-app document
// flows (PhoneDocuments).

@main
struct VcadPhoneApp: App {
    @State private var model = EditorModel()
    @State private var intent = IntentEngine()

    var body: some Scene {
        WindowGroup {
            PhoneRootView(model: model, intent: intent)
                .task {
                    ReleaseWindowController.shared.show(model: model, intent: intent)
                    // A phone opens on the part, not on panels: the navigator
                    // and inspector come up on demand (⋯, or a selection).
                    model.showsTree = false
                    PhoneDocuments.installSamplesIfNeeded()
                    // Simulator/dev hook (simctl launch with SIMCTL_CHILD_VCAD_OPEN).
                    let env = ProcessInfo.processInfo.environment
                    if let path = env["VCAD_OPEN"], !path.isEmpty {
                        PhoneDocuments.open(URL(fileURLWithPath: path), into: model)
                    }
                    if PhoneDemo.enabled { await PhoneDemo.run(model: model) }
                }
                // "Open in vcad" from Files / share sheets.
                .onOpenURL { url in PhoneDocuments.open(url, into: model) }
        }
    }
}
