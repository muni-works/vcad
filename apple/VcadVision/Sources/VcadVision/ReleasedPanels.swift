import SwiftUI

// The released tool windows. On the Mac, release-to-desktop scatters a
// Component Palette and an Object Inspector over the desktop as their own
// floating windows; here they are their own visionOS windows, floating beside
// the parts in the room. Both drive the same EditorModel as the volume.

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
        openWindow(id: ReleaseScene.palette)
        // The inspector places itself beside the palette, so the palette has
        // to exist first.
        try? await Task.sleep(nanoseconds: 400_000_000)
        openWindow(id: ReleaseScene.inspector)
        dismissWindow(id: ReleaseScene.volume)
    }

    func returnToWindow() async {
        openWindow(id: ReleaseScene.volume)
        await dismissImmersiveSpace()
        dismissWindow(id: ReleaseScene.inspector)
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

/// The volume's "Release" button: lift the parts out into the room.
struct ReleaseButton: View {
    var body: some View {
        ReleaseActionsReader { actions in
            Button {
                Task { await actions.release() }
            } label: {
                Label("Release into Room", systemImage: "arrow.up.forward.app")
                    .font(.system(size: 13))
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .frame(width: 230)
        }
    }
}

/// The Component Palette: tools, the model tree, and the room controls
/// (real-scale reset, return to window).
struct ComponentPaletteWindow: View {
    @Bindable var model: EditorModel
    @Bindable var room = ReleasedRoom.shared

    var body: some View {
        ReleaseActionsReader { actions in
            VStack(alignment: .leading, spacing: Theme.Space.m) {
                HStack {
                    PanelHeader(title: "Component Palette")
                    Spacer()
                }
                ToolPaletteView(model: model)
                FeatureTreeView(model: model)
                    .frame(minHeight: 220, maxHeight: 360)
                Divider()
                HStack(spacing: Theme.Space.s) {
                    Text(scaleLabel)
                        .font(.callout.monospacedDigit())
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("Reset Layout") { room.reset() }
                        .help("Put every part back and return to 1:1")
                    Button {
                        Task { await actions.returnToWindow() }
                    } label: {
                        Label("Return to Window", systemImage: "arrow.down.backward.square")
                    }
                    .buttonStyle(.borderedProminent)
                }
            }
            .padding(Theme.Space.l)
            .frame(width: 420)
            .task {
                guard let secs = room.scriptedReturn else { return }
                room.scriptedReturn = nil
                try? await Task.sleep(nanoseconds: UInt64(secs * 1e9))
                await actions.returnToWindow()
            }
        }
        .modifier(ReturnsWhenOrphaned(id: ReleaseScene.palette))
    }

    private var scaleLabel: String {
        abs(room.scale - 1) < 0.005 ? "Real scale (1:1)" : String(format: "Scale %.2f×", room.scale)
    }
}

/// The Object Inspector — the Mac's own released inspector view
/// (`ObjectInspectorWindow`, shared from ReleasedDesktop.swift), or the
/// receipt ledger for the gripper demo, as on the Mac.
struct ObjectInspectorPanel: View {
    @Bindable var model: EditorModel

    var body: some View {
        Group {
            if model.source.isGripper {
                ReceiptLedgerWindow(model: model)
            } else {
                ObjectInspectorWindow(model: model)
            }
        }
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
                // as the palette's own Return button.
                ReleasedRoom.shared.scriptedReturn = env["VCAD_RELEASE_RETURN"].flatMap(Double.init)
                try? await Task.sleep(nanoseconds: 1_500_000_000)
                await actions.release()
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
