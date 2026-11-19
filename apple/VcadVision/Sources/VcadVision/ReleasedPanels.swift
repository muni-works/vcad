import SwiftUI

// The released tool windows. On the Mac, release-to-desktop floats the
// editor's own chrome over the desktop around the parts: the toolbar and the
// dock, the left rail (model navigator) and the right rail (inspector). Here
// the very same views (GlassToolbar, DockHost, ReleasedLeftRail,
// ReleasedRightRail — shared Mac code) are visionOS windows floating beside
// the parts in the room. All drive the same EditorModel as the volume.

/// Release / return, shared by the volume's button, the palette's button and
/// the launch hook. The Mac hides the main window while released and brings it
/// back on return; here the volume closes and reopens the same way.
@MainActor
struct ReleaseActions {
    let openImmersiveSpace: OpenImmersiveSpaceAction
    let dismissImmersiveSpace: DismissImmersiveSpaceAction
    let openWindow: OpenWindowAction
    let dismissWindow: DismissWindowAction

    func release() async {
        ReleasedRoom.shared.reset()
        guard case .opened = await openImmersiveSpace(id: ReleaseScene.space) else { return }
        ReleaseWindowController.shared.editorModel?.isWindowed = false
        openWindow(id: ReleaseScene.palette)
        // The rails place themselves either side of the toolbar window, so it
        // has to exist first.
        try? await Task.sleep(nanoseconds: 400_000_000)
        openWindow(id: ReleaseScene.navigator)
        openWindow(id: ReleaseScene.inspector)
        dismissWindow(id: ReleaseScene.volume)
    }

    func returnToWindow() async {
        ReleaseWindowController.shared.editorModel?.isWindowed = true
        openWindow(id: ReleaseScene.volume)
        await dismissImmersiveSpace()
        dismissWindow(id: ReleaseScene.inspector)
        dismissWindow(id: ReleaseScene.navigator)
        dismissWindow(id: ReleaseScene.palette)
    }
}

/// Reads the scene-management actions out of the environment.
private struct ReleaseActionsReader<Content: View>: View {
    @Environment(\.openImmersiveSpace) private var openImmersiveSpace
    @Environment(\.dismissImmersiveSpace) private var dismissImmersiveSpace
    @Environment(\.openWindow) private var openWindow
    @Environment(\.dismissWindow) private var dismissWindow
    let content: (ReleaseActions) -> Content

    var body: some View {
        content(ReleaseActions(openImmersiveSpace: openImmersiveSpace,
                               dismissImmersiveSpace: dismissImmersiveSpace,
                               openWindow: openWindow, dismissWindow: dismissWindow))
    }
}

/// A released panel restored without its room (visionOS scene restoration
/// at launch, or the space dismissed from outside the app): reopen the volume
/// and close the panel, so the person is never left with tools and no part.
private struct ReturnsWhenOrphaned: ViewModifier {
    let id: String
    @Environment(\.openWindow) private var openWindow
    @Environment(\.dismissWindow) private var dismissWindow

    func body(content: Content) -> some View {
        content.task {
            try? await Task.sleep(nanoseconds: 1_200_000_000)
            guard !ReleasedRoom.shared.isReleased else { return }
            openWindow(id: ReleaseScene.volume)
            dismissWindow(id: id)
        }
    }
}

/// Runs the toolbar's Release to Desktop / Open in Window request
/// (`ReleaseWindowController.setWindowed`, shared GlassToolbar button) from
/// whichever scene carries it: the volume releases, the toolbar window returns.
struct ReleaseTransitions: ViewModifier {
    /// The request this scene acts on: false = release (volume), true =
    /// return (the released toolbar window).
    let handles: Bool
    @Bindable private var room = ReleasedRoom.shared

    func body(content: Content) -> some View {
        ReleaseActionsReader { actions in
            content.onChange(of: room.requestedWindowed) { _, want in
                guard want == handles else { return }
                room.requestedWindowed = nil
                Task { handles ? await actions.returnToWindow() : await actions.release() }
            }
        }
    }
}

/// The released toolbar window: the Mac's GlassToolbar over its command,
/// palette and toast hosts, the dock at the bottom, and the room's own row
/// (scale, reset). The toolbar's Open in Window button is the return path.
struct ReleasedToolbarWindow: View {
    @Bindable var model: EditorModel
    @Bindable var room = ReleasedRoom.shared

    var body: some View {
        ReleaseActionsReader { actions in
            VStack(spacing: Theme.Space.s) {
                GlassToolbar(model: model)
                ToastHost()
                PartFailureHost(model: model)
                CommandPanelHost(model: model)
                FloatingPanelHost()
                PaletteHost(model: model)
                DockHost(model: model)
                HStack(spacing: Theme.Space.s) {
                    Text(scaleLabel).font(.callout.monospacedDigit()).foregroundStyle(.secondary)
                    Spacer()
                    Button("Reset Layout") { room.reset() }
                        .help("Put every part back and return to 1:1")
                }
                .padding(.horizontal, Theme.Space.l)
            }
            .padding(.vertical, Theme.Space.m)
            .frame(width: 1120)
            .task {
                guard let secs = room.scriptedReturn else { return }
                room.scriptedReturn = nil
                try? await Task.sleep(nanoseconds: UInt64(secs * 1e9))
                ReleaseWindowController.shared.setWindowed(true)   // the toolbar button's path
            }
        }
        .modifier(ReleaseTransitions(handles: true))
        .modifier(ReturnsWhenOrphaned(id: ReleaseScene.palette))
    }

    private var scaleLabel: String {
        abs(room.scale - 1) < 0.005 ? "Real scale (1:1)" : String(format: "Scale %.2f×", room.scale)
    }
}

/// The released left rail window — the Mac's own `ReleasedLeftRail`.
struct ReleasedNavigatorWindow: View {
    @Bindable var model: EditorModel

    var body: some View {
        ReleasedLeftRail(model: model)
            .macPanelControls()
            .padding(Theme.Space.s)
            .modifier(ReturnsWhenOrphaned(id: ReleaseScene.navigator))
    }
}

/// The released right rail window — the Mac's own `ReleasedRightRail`
/// (Object Inspector, or whichever panel takes its place).
struct ReleasedInspectorWindow: View {
    @Bindable var model: EditorModel

    var body: some View {
        ReleasedRightRail(model: model)
            .macPanelControls()
            .padding(Theme.Space.s)
            .modifier(ReturnsWhenOrphaned(id: ReleaseScene.inspector))
    }
}

/// Dev hook (the visionOS twin of the Mac's `VCAD_WINDOWED`): with
/// `VCAD_RELEASE=1` the app releases straight into the room on launch;
/// `VCAD_RELEASE_SELECT=<part>` selects a part and `VCAD_RELEASE_MOVE=
/// <part>:<x>,<y>,<z>` carries it by that many millimetres, and
/// `VCAD_RELEASE_RETURN=<seconds>` returns to the window after that long —
/// scripted stand-ins for look-and-pinch when driving the simulator headless.
struct ReleaseLaunchHook: ViewModifier {
    let model: EditorModel
    /// Once per launch: the volume reappears on return and must not re-release.
    @MainActor private static var fired = false

    func body(content: Content) -> some View {
        ReleaseActionsReader { actions in
            content.task {
                let env = ProcessInfo.processInfo.environment
                guard env["VCAD_RELEASE"] == "1", !Self.fired else { return }
                Self.fired = true
                // The return runs from the palette (this volume closes on
                // release, taking its scene actions with it) — the same path
                // as the toolbar's Open in Window button.
                ReleasedRoom.shared.scriptedReturn = env["VCAD_RELEASE_RETURN"].flatMap(Double.init)
                try? await Task.sleep(nanoseconds: 1_500_000_000)
                ReleaseWindowController.shared.setWindowed(false)  // the toolbar button's path
                try? await Task.sleep(nanoseconds: 1_500_000_000)
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                if let s = env["VCAD_RELEASE_SELECT"], let pi = Int(s),
                   let fid = model.featureNodes.first(where: { $0.partIndex == pi })?.id {
                    model.selectFeature(fid)
                }
                if let mv = env["VCAD_RELEASE_MOVE"] {
                    let parts = mv.split(separator: ":")
                    let xyz = parts.count == 2 ? parts[1].split(separator: ",").compactMap { Float($0) } : []
                    if parts.count == 2, xyz.count == 3 {
                        ReleasedRoom.shared.offsets["part\(parts[0])"] = SIMD3(xyz[0], xyz[1], xyz[2])
                        ReleasedRoom.shared.revision += 1
                    }
                }
            }
        }
    }
}
