import SwiftUI
import RealityKit
#if canImport(AppKit)
import AppKit
#endif
import simd
import CoreGraphics
import UniformTypeIdentifiers

// The Mac shell's entry point, menu bar, file panels, and the SwiftUI pieces
// every workspace shares: the feature tree, the parameter editors, the scrub
// field, the sketch bars and the playback transport. The window itself is
// built in ReleasedDesktop.swift; the Design workspace's panels live in
// DesignShell.swift. Styling comes from Theme.swift, scene assets from
// SceneAssets.swift.

#if os(macOS)  // mac window root + document menu
/// SwiftUI lifecycle host. The visible editor lives in a single AppKit window
/// that can switch between a desktop overlay and a normal resizable window.
struct EditorView: View {
    let model: EditorModel
    let intent: IntentEngine

    var body: some View {
        Color.clear
            .frame(width: 1, height: 1)
            .background(HostWindowHider {
                ReleaseWindowController.shared.show(model: model, intent: intent)
            })
            .task {
                    // Dev hook: VCAD_GRIPPER=1 [VCAD_CONNECTOR_X=n] launches into
                    // the cross-domain gripper (used to verify without driving the UI).
                    let env = ProcessInfo.processInfo.environment
                    AppInstance.currentModel = model
                    // Offline native CNC smoke hook. Never contacts hardware.
                    if env["VCAD_CNC_DEMO"] == "1" {
                        model.workspace = .manufacture
                        model.cnc.select(.operation(model.cnc.selectedOperation.id))
                        model.cnc.generate()
                        model.cnc.machine.connect(simulated: true)
                    }
                    // Pick up documents other instances opened while this one
                    // was already running.
                    model.refreshRecents()
                    LaunchPerf.once("shell appeared")
                    // The document this instance was launched for: the argument
                    // a spawning instance passed, the VCAD_OPEN dev hook, or a
                    // path on the command line.
                    if let url = AppInstance.launchDocument() {
                        model.openDocument(url)
                    }
                    // Files that arrived before this view existed: the first
                    // document takes this (scratch) instance, outlines join it,
                    // any further documents get instances of their own.
                    for url in AppInstance.pendingOpen { AppInstance.opening(url, from: model) }
                    AppInstance.pendingOpen = []
                    // First launch: samples and the loon tutorial.
                    if WelcomeWindowController.shouldShowAtLaunch(model: model) {
                        WelcomeWindowController.shared.show(model: model)
                    }
                    // Dev hook: VCAD_SIM=1 builds the physics simulation for the
                    // opened document and starts it running, so the whole path
                    // (enableSimulation -> prepare -> stepping engine -> instance
                    // transforms -> renderer) can be verified without driving the
                    // UI. Same purpose as VCAD_GRIPPER/VCAD_ROUTE below.
                    if env["VCAD_SIM"] == "1" {
                        // After the geometry rebuild, or there is no resident
                        // assembly scene to bind against yet.
                        try? await Task.sleep(for: .seconds(2))
                        model.enableSimulation()
                        if model.sim.isReady {
                            model.sim.run()
                        }
                        // VCAD_POLICY=<path> drives with a trained policy.
                        if let pol = env["VCAD_POLICY"], !pol.isEmpty {
                            model.sim.loadPolicy(from: URL(fileURLWithPath: pol))
                        }
                        // VCAD_TRAIN=1 additionally kicks off a short ARS run,
                        // so the training console can be seen working.
                        if env["VCAD_TRAIN"] == "1" {
                            model.sim.trainSpec.ars.iterations = 12
                            model.sim.trainSpec.ars.n_directions = 4
                            model.sim.trainSpec.ars.top_k = 2
                            model.sim.trainSpec.held_out_seeds = 3
                            model.sim.trainSpec.held_out_every = 1
                            model.sim.spec.max_steps = 60
                            model.sim.startTraining()
                        }
                        let line = "[VCAD_SIM] available=\(model.sim.isAvailable) "
                            + "ready=\(model.sim.isReady) actionDim=\(model.sim.actionDim) "
                            + "error=\(model.sim.errorMessage ?? "none")\n"
                        FileHandle.standardError.write(Data(line.utf8))
                    }
                    guard env["VCAD_GRIPPER"] == "1" else { return }
                    model.openGripper()
                    if let x = env["VCAD_CONNECTOR_X"].flatMap(Double.init) { model.connectorX = x }
                    if env["VCAD_ROUTE"] == "1" {
                        let segs = model.routeGripperCopper()
                        // stderr is unbuffered → survives the kill that a buffered
                        // stdout print would lose; a quick end-to-end FFI smoke test.
                        let line = "[VCAD_ROUTE] connector_x=\(Int(model.connectorX)) "
                            + "segments=\(segs.count) unrouted=\(model.copperUnrouted)\n"
                        FileHandle.standardError.write(Data(line.utf8))
                    }
            }
    }
}

/// Hides the WindowGroup's window and opens the released overlay in its place.
/// Done from an NSViewRepresentable rather than a `.task` because the overlay
/// controller needs the host window to exist (it hides it), and a view's window
/// is nil until AppKit has attached it — the old code waited out that race with
/// a 1s sleep, which is exactly the studio flash this removes.
struct HostWindowHider: NSViewRepresentable {
    /// Called once, on the main actor, as soon as the host window exists.
    let onAttach: () -> Void

    func makeNSView(context: Context) -> NSView { HiderView(onAttach: onAttach) }
    func updateNSView(_ view: NSView, context: Context) {}

    final class HiderView: NSView {
        private let onAttach: () -> Void
        private var fired = false
        init(onAttach: @escaping () -> Void) {
            self.onAttach = onAttach
            super.init(frame: .zero)
        }
        required init?(coder: NSCoder) { fatalError("unused") }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            guard let w = window, !fired else { return }
            fired = true
            // Zero alpha first: orderOut alone can still flash a frame if the
            // window was ordered in earlier in this pass.
            w.alphaValue = 0
            w.isRestorable = false
            // Opening a file (Finder, `open`, Dock) makes SwiftUI order the
            // WindowGroup window back in — at alpha 0 it is invisible but
            // still on top, swallowing every click meant for the document
            // window. Never take mouse events, and send it straight back out
            // (handing key to the real window) whenever it resurfaces.
            w.ignoresMouseEvents = true
            w.orderOut(nil)
            for name in [NSWindow.didBecomeKeyNotification, NSWindow.didBecomeMainNotification] {
                observers.append(NotificationCenter.default.addObserver(
                    forName: name, object: w, queue: .main) { _ in
                    MainActor.assumeIsolated {
                        w.orderOut(nil)
                        ReleaseWindowController.shared.window?.makeKeyAndOrderFront(nil)
                    }
                })
            }
            onAttach()
        }
        // Lives as long as the app's one WindowGroup window; never removed.
        private var observers: [NSObjectProtocol] = []
    }
}

// MARK: - Menu bar

/// The app's real menu bar. Every action the app can take is here — so the
/// Help menu's search finds it, every key equivalent works no matter which
/// floating panel has focus, and nothing hides behind a bare key.
///
/// Key equivalents keep clear of the system's: ⌘W closes, ⇧⌘W is not
/// re-bound, ⌃-digits stay with Mission Control, and nothing fires on an
/// unmodified letter.
struct DocumentCommands: Commands {
    @Bindable var model: EditorModel
    @Bindable var intent: IntentEngine
    @AppStorage(Prefs.gridKey) private var showsGrid = true
    @AppStorage(Prefs.sketchGridKey) private var showsSketchGrid = true

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            // ⌘N opens another COPY OF THE APP, so the new document gets its own
            // Dock tile and its own ⌘-Tab entry (see AppInstance for why that
            // cannot be a second window).
            Button("New Document") { AppInstance.open() }.keyboardShortcut("n")
            Divider()
            Button("Open…") { openPanel(model) }.keyboardShortcut("o")
            Button("Open Sample…") { WelcomeWindowController.shared.show(model: model) }
            Menu("Open Recent") {
                ForEach(model.recents, id: \.self) { url in
                    Button(url.deletingPathExtension().lastPathComponent) {
                        AppInstance.opening(url, from: model)
                    }
                }
                if !model.recents.isEmpty {
                    Divider()
                    Button("Clear Menu") { model.clearRecents() }
                }
            }
            .disabled(model.recents.isEmpty)
            Button("Import from Photos (Experimental)…") { PhotoImportController.shared.show(for: model) }
            if !model.examples.isEmpty {
                Menu("Open Example") {
                    ForEach(model.examples, id: \.path) { ex in
                        Button(ex.name) {
                            AppInstance.opening(URL(fileURLWithPath: ex.path), from: model)
                        }
                    }
                }
            }
        }
        CommandGroup(replacing: .saveItem) {
            Button("Close") { ReleaseWindowController.shared.requestClose() }
                .keyboardShortcut("w")
            Divider()
            Button("Save") { model.saveDocument() }
                .keyboardShortcut("s").disabled(!model.usesDocumentTree || !model.documentDirty)
            Button("Save As…") { saveAsPanel(model) }
                .keyboardShortcut("s", modifiers: [.command, .shift])
                .disabled(!model.usesDocumentTree)
            Button("Revert to Saved") { model.revertDocument() }
                .disabled(!model.usesDocumentTree || !model.documentDirty)
            Button("Save Version…") { VersionBrowserWindow.promptForNamedVersion(model) }
                .keyboardShortcut("s", modifiers: [.command, .option])
                .disabled(model.documentURL == nil || !model.usesDocumentTree)
            Button("Browse Versions…") { VersionBrowserWindow.show(model) }
                .disabled(!model.usesDocumentTree)
            Button("Show in Finder") { model.showInFinder() }
                .disabled(model.documentURL == nil)
            Divider()
            Menu("Export") {
                Button("STL…") { exportDocumentPanel(model, format: .stl) }
                    .keyboardShortcut("e").disabled(!model.canExport)
                Button("STEP…") { exportDocumentPanel(model, format: .step) }
                    .disabled(!model.canExportDocument)
                Button("GLB…") { exportDocumentPanel(model, format: .glb) }
                    .disabled(!model.canExportDocument)
                Button("USDZ…") { exportUSDZPanel(model) }
                    .keyboardShortcut("e", modifiers: [.command, .shift]).disabled(!model.canExport)
            }
            Menu("Export Drawing") {
                Button("DXF…") { DrawingStore.shared.model = model; DrawingStore.shared.export(.dxf) }
                Button("PDF…") { DrawingStore.shared.model = model; DrawingStore.shared.export(.pdf) }
                    .keyboardShortcut("p", modifiers: [.command, .option, .shift])
            }.disabled(!model.canExportDocument)
            Button("Render Photoreal…") { PhotorealRenderController.shared.show(for: model) }
                .keyboardShortcut("r", modifiers: [.command, .option, .shift])
                .disabled(!model.canExportDocument)
        }
        CommandGroup(replacing: .undoRedo) {
            Button(model.undoActionName.map { "Undo \($0)" } ?? "Undo") { model.undo() }
                .keyboardShortcut("z").disabled(!model.canUndo)
            Button(model.redoActionName.map { "Redo \($0)" } ?? "Redo") { model.redo() }
                .keyboardShortcut("z", modifiers: [.command, .shift]).disabled(!model.canRedo)
        }
        // The standard Edit ▸ Delete and Select All items reach the model
        // through the responder chain (KeyableWindow), so a text field keeps
        // them while it is first responder and the parts get them otherwise.
        CommandGroup(after: .pasteboard) {
            Button("Deselect All") { model.deselectAll() }
                .keyboardShortcut("a", modifiers: [.command, .shift]).disabled(!model.hasSelection)
            Divider()
            Button("Rename") { model.beginRenamingSelection() }
                .disabled(model.selectedFeatureNode == nil)
            Divider()
            Button(model.selectedPartIndex.map { model.isPartVisible($0) } ?? true ? "Hide" : "Show") {
                model.toggleSelectedVisibility()
            }
            .keyboardShortcut("h", modifiers: [.command, .shift]).disabled(model.selectedPartIndex == nil)
            Button(model.selectedPartIndex.map { model.isolatedPart == $0 } ?? false ? "Exit Isolate" : "Isolate") {
                model.toggleSelectedIsolate()
            }
            .keyboardShortcut("i", modifiers: [.command, .shift]).disabled(model.selectedPartIndex == nil)
            Button("Show All Parts") { model.showAllParts() }
                .keyboardShortcut("h", modifiers: [.command, .shift, .option]).disabled(!model.hasHiddenParts)
            Divider()
            if IntentEngine.isAvailable {
                Button("Describe a Part…") { intent.focusRequested = true }
                    .keyboardShortcut("k")
            }
            Button(AgentSession.shared.isOpen ? "Hide Agent" : "Show Agent") { AgentSession.shared.toggle() }
                .keyboardShortcut("k", modifiers: [.command, .shift])
        }
        CommandGroup(after: .sidebar) {
            Divider()
            ForEach(DockSpace.allCases) { ds in
                Toggle(ds.label, isOn: Binding(
                    get: { GlassUI.shared.space == ds },
                    set: { if $0 { GlassUI.shared.enter(ds, model: model) } }))
                    .keyboardShortcut(ds.key, modifiers: [.command, .control])
            }
            Divider()
            Toggle("Model Navigator", isOn: $model.showsTree)
                .keyboardShortcut("1", modifiers: [.command, .option])
            Toggle("Inspector", isOn: $model.showsInspector)
                .keyboardShortcut("2", modifiers: [.command, .option])
            Button(model.anyPanelShown ? "Hide All Panels" : "Show All Panels") {
                model.setPanels(shown: !model.anyPanelShown)
            }
            .keyboardShortcut("0", modifiers: [.command, .option])
            Divider()
            Toggle("Show Grid", isOn: $showsGrid)
                .keyboardShortcut("g", modifiers: [.command, .option])
            Toggle("Show Sketch Grid", isOn: $showsSketchGrid)
            Divider()
            Toggle("Zebra Analysis", isOn: $model.zebraMode)
                .keyboardShortcut("z", modifiers: [.command, .option])
            Toggle("Ray-Traced Preview", isOn: Binding(
                get: { model.raytraceEnabled },
                set: { model.setRaytrace($0) }))
                .keyboardShortcut("r", modifiers: [.command, .option])
                .disabled(!model.usesDocumentTree)
            Toggle("Path-Traced View", isOn: Binding(
                get: { model.pathTraceEnabled },
                set: { model.setPathTrace($0) }))
                .keyboardShortcut("p", modifiers: [.command, .option])
                .disabled(!model.usesDocumentTree && model.assemblyInstanceCount == 0)
            Divider()
            Button(model.isWindowed ? "Release to Desktop" : "Open in Window") {
                ReleaseWindowController.shared.setWindowed(!model.isWindowed)
            }
            .keyboardShortcut("d", modifiers: [.command, .shift])
        }
        CommandMenu("Tools") {
            Button("Find Command…") { model.showsCommandPalette = true }
                .keyboardShortcut("p", modifiers: [.command, .shift])
            Button("Parameters…") { model.showsParameterTable = true }
                .keyboardShortcut("p", modifiers: [.command, .option])
                .disabled(!model.usesDocumentTree)
            Divider()
            Toggle("Measure", isOn: $model.measuring)
                .keyboardShortcut("m", modifiers: [.control])
                .disabled(!model.usesDocumentTree)
            Toggle("Section View", isOn: Binding(get: { model.section != nil },
                                                 set: { _ in model.toggleSmartSection() }))
                .keyboardShortcut("x", modifiers: [.command, .option])
            Divider()
            Button("New Sketch") { if model.promoteSandboxToDocument(blank: true) { model.enterSketch() } }
                .keyboardShortcut("s", modifiers: [.control])
                .disabled(!model.canModel || model.sketching)
            Menu("Add Shape") {
                ForEach(BaseShape.allCases, id: \.self) { shape in
                    Button(shape.label) { if model.promoteSandboxToDocument(blank: true) { model.arm(shape) } }
                }
            }.disabled(!model.canModel)
            Menu("Feature") {
                ForEach(FeatureOp.allCases) { op in
                    Button(op.label + "…") { if model.promoteSandboxToDocument() { model.activeFeatureOp = op } }
                }
            }.disabled(!model.canModel)
            Menu("Combine") {
                ForEach([BooleanOp.union, .difference, .intersection], id: \.self) { op in
                    Button(op.label) { model.beginCombine(op) }
                }
            }.disabled(!model.usesDocumentTree || model.selectedPartIndex == nil)
        }
        CommandMenu("Select") {
            ForEach(PickFilter.allCases) { f in
                Toggle(f.label, isOn: Binding(
                    get: { model.picking.filter == f },
                    set: { if $0 { model.picking.filter = f } }))
                    .keyboardShortcut(f.shortcut, modifiers: [.control])
            }
            Divider()
            Button("Clear Face/Edge Selection") { model.picking.clear() }
                .disabled(model.picking.selection.isEmpty)
        }
        CommandMenu("Camera") {
            Button("Isometric") { model.animateCamera(to: .isometric) }.keyboardShortcut("1")
            Button("Front") { model.animateCamera(to: .front) }.keyboardShortcut("2")
            Button("Right") { model.animateCamera(to: .right) }.keyboardShortcut("3")
            Button("Top") { model.animateCamera(to: .top) }.keyboardShortcut("4")
            Divider()
            Button("Turn View Left") { model.turnView(clockwise: false) }
            Button("Turn View Right") { model.turnView(clockwise: true) }
            Divider()
            Menu("Views") {
                let views = NavigationUI.shared.views(for: model)
                ForEach(views) { v in
                    Button(v.name) { NavigationUI.shared.glide(model, to: v.pose) }
                }
                if views.isEmpty { Text("No saved views") }
            }
            Button("Save View…") { NavigationUI.shared.namingView = true }
                .keyboardShortcut("v", modifiers: [.command, .option])
            Divider()
            Button("Quick Actions") {
                NavigationUI.shared.openHalo(at: EditorModel.pointerInOverlay, model: model, marking: false)
            }
            .keyboardShortcut(".", modifiers: [.command, .option])
            Divider()
            Button("Frame All") { model.resetCamera(animated: true) }.keyboardShortcut("0")
            Button("Frame Selection") { ReleaseWindowController.shared.frameSelection() }
                .keyboardShortcut("0", modifiers: [.command, .option]).disabled(!model.hasSelection)
        }
        CommandGroup(replacing: .help) {
            Button("Keyboard Shortcuts") { GlassUI.shared.shortcutsShown.toggle() }
                .keyboardShortcut("/", modifiers: .command)
            Divider()
            Button("Welcome to vcad") { WelcomeWindowController.shared.show(model: model) }
            Button("Connect Agents…") { AgentInstallPrompt.run() }
            Divider()
            Button("vcad Help") { NSWorkspace.shared.open(URL(string: "https://vcad.io/docs")!) }
            Divider()
            Button("Report an Issue…") { NSWorkspace.shared.open(URL(string: "https://github.com/ecto/vcad/issues/new")!) }
        }
    }
}

// MARK: document panels (shared by the menu bar and the workspaces)

/// The `.vcad` document type, declared in the bundle's Info.plist and used by
/// every open/save panel. Falls back to a dynamic type when run as a bare
/// executable (swift run) with no bundle.
extension UTType {
    static let vcadDocument: UTType =
        UTType("io.vcad.document") ?? UTType(filenameExtension: "vcad") ?? .json
    static let loonSource: UTType =
        UTType("io.vcad.loon") ?? UTType(filenameExtension: "loon") ?? .plainText
}

@MainActor func exportSTLPanel(_ model: EditorModel) {
    savePanel(name: "\(model.documentName).stl", type: UTType(filenameExtension: "stl") ?? .data,
              prompt: "Export") {
        _ = model.exportSTL(to: $0)
    }
}

@MainActor func exportUSDZPanel(_ model: EditorModel) {
    savePanel(name: "\(model.documentName).usdz", type: .usdz, prompt: "Export") {
        _ = model.exportUSDZ(to: $0)
    }
}

@MainActor func saveAsPanel(_ model: EditorModel) {
    savePanel(name: "\(model.documentName).vcad", type: .vcadDocument, prompt: "Save") {
        model.saveDocumentAs($0)
    }
}

@MainActor func openPanel(_ model: EditorModel) {
    let panel = NSOpenPanel()
    panel.allowedContentTypes = [.vcadDocument, .loonSource, .folder] + ["step", "stp", "urdf"].compactMap { UTType(filenameExtension: $0) }
    // Several documents at once are several instances, so a multiple selection
    // is a perfectly good thing to ask for.
    panel.allowsMultipleSelection = true
    panel.canChooseDirectories = true     // a loon project folder opens its main.loon
    panel.prompt = "Open"
    present(panel) {
        for (i, url) in panel.urls.enumerated() {
            // The first one may claim this instance if it is a scratch; the rest
            // always get their own.
            if i == 0 { AppInstance.opening(url, from: model) } else { AppInstance.open(document: url) }
        }
    }
}

@MainActor private func savePanel(name: String, type: UTType, prompt: String,
                                  _ done: @escaping (URL) -> Void) {
    let panel = NSSavePanel()
    panel.allowedContentTypes = [type]
    panel.nameFieldStringValue = name
    panel.prompt = prompt
    panel.canCreateDirectories = true
    present(panel) { if let url = panel.url { done(url) } }
}

/// Run a file panel as a sheet on the editor window when there is one, and
/// app-modal only when released over the desktop (no window to hang it on).
@MainActor func present(_ panel: NSSavePanel, onOK: @escaping () -> Void) {
    if let host = ReleaseWindowController.shared.hostWindow,
       host.styleMask.contains(.titled) {
        panel.beginSheetModal(for: host) { resp in
            if resp == .OK { onOK() }
        }
    } else if panel.runModal() == .OK {
        onOK()
    }
}
#endif  // os(macOS) — mac window root + document menu

/// The leading wordmark.
struct BrandMark: View {
    var body: some View {
        Text("vcad")
            .font(.system(.body, design: .rounded, weight: .semibold))
            .foregroundStyle(.secondary)
            .padding(.trailing, 2)
    }
}

// MARK: - Feature tree

struct FeatureTreeView: View {
    @Bindable var model: EditorModel
    var embedded = false
    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            if !embedded { header }
            if model.usesDocumentTree {
                if model.featureNodes.isEmpty {
                    EmptyPanelState(title: "No features yet", systemImage: "cube.transparent",
                                    detail: "Add a primitive or sketch a profile to start.")
                } else {
                    ForEach(model.historyTreeItems) { item in
                        switch item {
                        case .node(let node, let row):
                            FeatureRowView(model: model, node: node, depth: 0, historyRow: row)
                        case .ghost(let row):
                            GhostFeatureRow(model: model, row: row)
                        case .marker:
                            RollbackMarkerRow(model: model)
                        }
                    }
                }
            } else {
                ForEach(model.features) { f in
                    let selected = model.selectedFeatureID == f.id
                    Button { model.selectedFeatureID = f.id } label: {
                        HStack(spacing: Theme.Space.s) {
                            Image(systemName: f.symbol).frame(width: 16)
                            Text(f.name)
                            Spacer(minLength: 0)
                        }
                        .font(.callout)
                        .padding(.horizontal, Theme.Space.s).padding(.vertical, 6)
                        .selectableRow(selected: selected)
                        .foregroundStyle(selected ? Color.primary : Color.secondary)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(selected ? .isSelected : [])
                }
            }
        }
        .padding(embedded ? 0 : 6)
        .modifier(DesignTreeSurface(embedded: embedded))
        .animation(Motion.snappy, value: model.selectedFeatureID)
        .animation(Motion.snappy, value: model.expandedFeatureIDs)
        .animation(Motion.snappy, value: model.hiddenParts)
        .animation(Motion.snappy, value: model.isolatedPart)
    }

    private var header: some View {
        HStack(spacing: 6) {
            Eyebrow(model.usesDocumentTree ? "Features" : "History")
            Spacer(minLength: 0)
            if model.hasHiddenParts {
                Button { model.showAllParts() } label: {
                    Label("Show all", systemImage: "eye").font(.caption.weight(.medium))
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .help("Show all hidden parts")
            }
        }
        .padding(.horizontal, Theme.Space.s).padding(.top, 6).padding(.bottom, 4)
    }
}

/// One row of the hierarchical document feature tree (recurses into operands
/// when expanded). Root rows carry an eye toggle + a context menu acting on the
/// part they produce.
struct FeatureRowView: View {
    @Bindable var model: EditorModel
    let node: FeatureNode
    let depth: Int
    /// The engine's history row for a top-level part (state, references).
    var historyRow: HistoryRow? = nil
    @State private var hovering = false

    private var brokenRows: [HistoryRow] { historyRow.map { model.brokenRows(under: $0) } ?? [] }
    private var broken: Bool { !brokenRows.isEmpty }

    private var expanded: Bool { model.expandedFeatureIDs.contains(node.id) }
    private var selected: Bool {
        if model.selectedFeatureID == node.id { return true }
        if let pi = node.partIndex { return model.multiSelectedParts.contains(pi) }
        return false
    }
    private var dimmed: Bool {
        guard let pi = node.partIndex else { return false }
        return !model.isPartVisible(pi)
    }
    /// Hovered via the tree row itself OR via the viewport (bidirectional).
    /// `hoveredFeatureID` covers assembly instances, whose rows are part defs
    /// and therefore have no part index of their own.
    private var hovered: Bool {
        hovering
            || model.hoveredFeatureID == node.id
            || (node.partIndex != nil && node.partIndex == model.hoveredPartIndex)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            row
                // Addressable by the ScrollViewReader that keeps the selected
                // row in view when the selection comes from the viewport.
                .id(node.id)
            if let h = historyRow, broken, model.repairingFeatureID == h.id {
                BrokenReferenceCard(model: model, rows: brokenRows)
            }
            if expanded {
                ForEach(node.children) { child in
                    FeatureRowView(model: model, node: child, depth: depth + 1)
                }
            }
        }
    }

    private var row: some View {
        HStack(spacing: 6) {
            if node.hasChildren {
                Button { model.toggleExpanded(node.id) } label: {
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.semibold))
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                        .foregroundStyle(.tertiary)
                        .frame(width: 12, height: 12)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(expanded ? "Collapse" : "Expand")
            } else {
                Color.clear.frame(width: 12, height: 12)
            }
            Image(systemName: node.symbol)
                .font(.callout).frame(width: 16)
                .foregroundStyle(selected ? AnyShapeStyle(Color.accentColor) : AnyShapeStyle(.secondary))
            VStack(alignment: .leading, spacing: 0) {
                if model.renamingFeatureID == node.id {
                    RenameField(initial: node.name,
                                commit: { model.renameFeature(node.nodeId, to: $0); model.renamingFeatureID = nil },
                                cancel: { model.renamingFeatureID = nil })
                } else {
                    Text(node.name).font(.callout).lineLimit(1)
                }
                if let d = node.detail {
                    Text(d).font(.caption.monospacedDigit())
                        .foregroundStyle(.tertiary).lineLimit(1)
                }
            }
            Spacer(minLength: 0)
            if let h = historyRow, broken {
                Button { model.repairingFeatureID = model.repairingFeatureID == h.id ? nil : h.id } label: {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
                }
                .buttonStyle(.plain)
                .help("A reference this feature is built on no longer resolves — click to repair")
                .accessibilityLabel("Broken reference, repair")
            }
            if let pi = node.partIndex { eyeButton(pi) }
        }
        .padding(.leading, CGFloat(depth) * Theme.Space.m + Theme.Space.xs)
        .padding(.trailing, 5).padding(.vertical, 4)
        .selectableRow(selected: selected, hovered: hovered)
        .foregroundStyle(selected ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
        .opacity(dimmed ? 0.45 : 1)
        .contentShape(Rectangle())
        .onHover { inside in
            hovering = inside
            // Row → viewport: light the geometry this row drew.
            if inside { model.hoverFeature(node) }
            else if model.hoveredFeatureID == node.id { model.hoverFeature(nil) }
        }
        // ⌘-click a part row → toggle it in the multi-selection (for booleans).
        // The modifier comes from the gesture, not NSEvent's hardware state.
        .gesture(TapGesture().modifiers(.command).onEnded {
            guard model.renamingFeatureID != node.id else { return }
            if let pi = node.partIndex { model.toggleMultiSelect(part: pi, featureID: node.id) }
            else { model.selectFeature(node.id) }
        })
        .gesture(TapGesture().onEnded {
            guard model.renamingFeatureID != node.id else { return }
            model.selectFeature(node.id)
        })
        .modifier(HistoryRowDrag(model: model, id: depth == 0 ? historyRow?.id : nil))
        .contextMenu { menu }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(node.detail.map { "\(node.name), \($0)" } ?? node.name)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private func eyeButton(_ pi: Int) -> some View {
        let vis = model.isPartVisible(pi)
        return Button { model.toggleVisibility(part: pi) } label: {
            Image(systemName: vis ? "eye" : "eye.slash")
                .font(.subheadline)
                .foregroundStyle(vis ? AnyShapeStyle(.tertiary) : AnyShapeStyle(.secondary))
                .frame(width: 18, height: 18)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // Visible parts keep the toggle quiet until the row is hovered; a
        // hidden part always shows its state.
        .opacity(vis && !hovered && !selected ? 0 : 1)
        .help(vis ? "Hide part" : "Show part")
        .accessibilityLabel(vis ? "Hide part" : "Show part")
    }

    @ViewBuilder private var menu: some View {
        if let pi = node.partIndex {
            Button { model.isolate(part: pi) } label: {
                Label(model.isolatedPart == pi ? "Exit Isolate" : "Isolate",
                      systemImage: "scope")
            }
            Button { model.toggleVisibility(part: pi) } label: {
                Label(model.isPartVisible(pi) ? "Hide" : "Show",
                      systemImage: model.isPartVisible(pi) ? "eye.slash" : "eye")
            }
            if model.hasHiddenParts {
                Button { model.showAllParts() } label: { Label("Show All", systemImage: "eye") }
            }
            Divider()
        }
        if node.hasChildren {
            Button { model.toggleExpanded(node.id) } label: {
                Label(expanded ? "Collapse" : "Expand", systemImage: "list.bullet.indent")
            }
        }
        Button { model.renamingFeatureID = node.id } label: { Label("Rename", systemImage: "pencil") }
        if let pi = node.partIndex, depth == 0 {
            Button { model.reorderPart(pi, to: pi - 1) } label: { Label("Move Up", systemImage: "arrow.up") }
                .disabled(pi == 0)
            Button { model.reorderPart(pi, to: pi + 1) } label: { Label("Move Down", systemImage: "arrow.down") }
                .disabled(pi >= model.featureNodes.count - 1)
            if let h = historyRow {
                Button { model.setFeatureSuppressed(h.id, true) } label: {
                    Label("Suppress", systemImage: "nosign")
                }
                .help("Keep the feature but don't build it; features built on it are skipped")
                Button { model.rollBack(after: h.id) } label: {
                    Label("Roll Back to Here", systemImage: "backward.end")
                }
                .disabled(h.marker)
                if model.isRolledBack {
                    Button { model.rollBack(after: nil) } label: { Label("Roll to End", systemImage: "forward.end.alt") }
                }
                if broken {
                    Button { model.repairingFeatureID = h.id } label: {
                        Label("Repair Reference…", systemImage: "exclamationmark.triangle")
                    }
                }
            }
            Button { model.toggleSuppressed(part: pi) } label: {
                Label(model.isSuppressed(part: pi) ? "Show in Document" : "Hide in Document",
                      systemImage: model.isSuppressed(part: pi) ? "eye" : "eye.slash")
            }
            .help("Saved with the file: the part stays in the document but isn't drawn")
        }
        Button {
            #if os(macOS)
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(node.name, forType: .string)
            #else
            UIPasteboard.general.string = node.name
            #endif
        } label: { Label("Copy Name", systemImage: "doc.on.doc") }
        if let pi = node.partIndex {
            Divider()
            Button(role: .destructive) { model.deletePart(pi) } label: {
                Label("Delete Part", systemImage: "trash")
            }
        }
    }
}

/// The tree's surface when it is not embedded in a navigator panel (visionOS).
struct DesignTreeSurface: ViewModifier {
    var embedded: Bool
    @ViewBuilder func body(content: Content) -> some View {
        if embedded { content } else { content.panelSurface() }
    }
}

// MARK: - Parameter editors

/// Scrub/stepper editors for a feature node's parameters — each writes back
/// into the live document and re-evaluates (parity with the web app's scrub
/// inputs). Shared by every inspector so a new editable op appears everywhere
/// at once.
struct FeatureParamEditors: View {
    @Bindable var model: EditorModel
    let node: FeatureNode
    /// Called after every edit, so the released overlay can mesh-swap in place.
    /// `snapshot` is true on a typed commit or the first tick of a scrub.
    var onEdit: (Bool) -> Void = { _ in }

    /// Op types that expose live-editable parameters in the inspector.
    static let editableOps: Set<String> = [
        "Cube", "Cylinder", "Sphere", "Cone", "Torus", "Wedge", "Prism", "SpurGear", "Hull", "SweepRotate",
        "EdgeBlend", "Fillet", "Chamfer", "Shell",
        "Translate", "Rotate", "Scale", "Revolve", "LinearPattern", "CircularPattern",
    ]

    /// Scrub sensitivity for a document parameter: span-derived when the doc
    /// declares a range (≈200 ticks across it), else the default 0.1 mm/pt.
    static func paramSensitivity(_ p: DocParameter) -> Double {
        if let lo = p.min, let hi = p.max, hi > lo { return (hi - lo) / 200 }
        return 0.1
    }

    var body: some View { editors }

    @ViewBuilder private var editors: some View {
        let op = model.opDict(nodeId: node.nodeId) ?? [:]
        let id = node.nodeId
        switch node.opType {
        case "Cube":
            axisField(op, id, "Width", "size", "x", minV: 0.1)
            axisField(op, id, "Depth", "size", "y", minV: 0.1)
            axisField(op, id, "Height", "size", "z", minV: 0.1)
        case "Cylinder":
            scalarField(op, id, "Radius", "radius"); scalarField(op, id, "Height", "height")
        case "Sphere":
            scalarField(op, id, "Radius", "radius")
        case "Cone":
            scalarField(op, id, "Bottom radius", "radius_bottom", minV: 0)
            scalarField(op, id, "Top radius", "radius_top", minV: 0)
            scalarField(op, id, "Height", "height")
        case "Torus":
            scalarField(op, id, "Major radius", "major_radius")
            scalarField(op, id, "Minor radius", "minor_radius")
        case "Wedge":
            axisField(op, id, "Width", "size", "x", minV: 0.1)
            axisField(op, id, "Depth", "size", "y", minV: 0.1)
            axisField(op, id, "Height", "size", "z", minV: 0.1)
        case "Prism":
            sidesStepper(id, (op["sides"] as? NSNumber)?.intValue ?? 6)
            scalarField(op, id, "Radius", "radius")
            scalarField(op, id, "Height", "height")
        case "SpurGear":
            scalarField(op, id, "Module", "module", sens: 0.01, minV: 0.05)
            teethStepper(id, (op["teeth"] as? NSNumber)?.intValue ?? 20)
            scalarField(op, id, "Pressure angle", "pressure_angle_deg", unit: "°", sens: 0.1, minV: 10)
            scalarField(op, id, "Face width", "face_width")
            scalarField(op, id, "Bore", "bore_diameter", minV: 0)
            scalarField(op, id, "Backlash", "backlash", sens: 0.005, minV: 0)
        case "Hull":
            HullInputsEditor(model: model, nodeId: id)
        case "SweepRotate":
            scalarField(op, id, "From", "angle_lo_deg", unit: "°", sens: 0.5, minV: -3600)
            scalarField(op, id, "To", "angle_hi_deg", unit: "°", sens: 0.5, minV: -3600)
            scalarField(op, id, "Clearance", "clearance", sens: 0.05, minV: 0)
        case "EdgeBlend":
            blendField(op, id, "Size", "size", sens: 0.05, minV: 0)
            blendField(op, id, "Shape", "shape", unit: "", sens: 0.01, minV: 0)
        case "Fillet":
            scalarField(op, id, "Radius", "radius", sens: 0.05, minV: 0)
        case "Chamfer":
            scalarField(op, id, "Distance", "distance", sens: 0.05, minV: 0)
        case "Shell":
            scalarField(op, id, "Thickness", "thickness", sens: 0.05, minV: 0.1)
        case "Translate":
            axisField(op, id, "X", "offset", "x"); axisField(op, id, "Y", "offset", "y")
            axisField(op, id, "Z", "offset", "z")
        case "Rotate":
            axisField(op, id, "X", "angles", "x", unit: "°", sens: 0.5)
            axisField(op, id, "Y", "angles", "y", unit: "°", sens: 0.5)
            axisField(op, id, "Z", "angles", "z", unit: "°", sens: 0.5)
        case "Scale":
            axisField(op, id, "X", "factor", "x", unit: "", sens: 0.01, minV: 0.01)
            axisField(op, id, "Y", "factor", "y", unit: "", sens: 0.01, minV: 0.01)
            axisField(op, id, "Z", "factor", "z", unit: "", sens: 0.01, minV: 0.01)
        case "Revolve":
            scalarField(op, id, "Angle", "angle_deg", unit: "°", sens: 0.5, minV: 0)
        case "LinearPattern":
            countStepper(id, (op["count"] as? NSNumber)?.intValue ?? 0)
        case "CircularPattern":
            countStepper(id, (op["count"] as? NSNumber)?.intValue ?? 0)
            scalarField(op, id, "Span", "angle_deg", unit: "°", sens: 0.5, minV: 0)
        default:
            EmptyView()
        }
    }

    private func scalarField(_ op: [String: Any], _ id: Int, _ label: String, _ key: String,
                             unit: String = "mm", sens: Double = 0.1, minV: Double = 0.1) -> some View {
        let value = (op[key] as? NSNumber)?.doubleValue ?? 0
        return ScrubField(label: label, value: value, unit: unit, sensitivity: sens, minValue: minV) { v, s in
            model.editScalar(nodeId: id, key: key, value: v, snapshot: s, name: "Change \(label)")
            onEdit(s)
        }
    }

    private func axisField(_ op: [String: Any], _ id: Int, _ label: String, _ key: String, _ a: String,
                           unit: String = "mm", sens: Double = 0.1,
                           minV: Double = -.greatestFiniteMagnitude) -> some View {
        let value = ((op[key] as? [String: Any])?[a] as? NSNumber)?.doubleValue ?? 0
        return ScrubField(label: label, value: value, unit: unit, sensitivity: sens, minValue: minV) { v, s in
            model.editVec(nodeId: id, key: key, axis: a, value: v, snapshot: s, name: "Change \(label)")
            onEdit(s)
        }
    }

    private func sidesStepper(_ id: Int, _ sides: Int) -> some View {
        Stepper(value: Binding(
            get: { sides },
            set: {
                model.editInt(nodeId: id, key: "sides", value: max(3, $0), snapshot: true, name: "Change Sides")
                onEdit(true)
            }
        ), in: 3...64) {
            KeyValueRow("Sides", "\(sides)")
        }
    }

    private func blendField(_ op: [String: Any], _ id: Int, _ label: String, _ key: String,
                            unit: String = "mm", sens: Double, minV: Double) -> some View {
        let value = ((op["profile"] as? [String: Any])?[key] as? NSNumber)?.doubleValue ?? 0
        return ScrubField(label: label, value: value, unit: unit, sensitivity: sens, minValue: minV) { v, s in
            model.editBlend(nodeId: id, field: key, value: v, snapshot: s)
            onEdit(s)
        }
    }

    private func teethStepper(_ id: Int, _ teeth: Int) -> some View {
        Stepper(value: Binding(
            get: { teeth },
            set: {
                model.editInt(nodeId: id, key: "teeth", value: max(4, $0), snapshot: true, name: "Change Teeth")
                onEdit(true)
            }
        ), in: 4...400) {
            KeyValueRow("Teeth", "\(teeth)")
        }
    }

    private func countStepper(_ id: Int, _ count: Int) -> some View {
        Stepper(value: Binding(
            get: { count },
            set: {
                model.editInt(nodeId: id, key: "count", value: max(1, $0), snapshot: true, name: "Change Count")
                onEdit(true)
            }
        ), in: 1...200) {
            KeyValueRow("Count", "\(count)")
        }
    }
}

/// A numeric field — the native take on the web app's scrub inputs. Drag the
/// value horizontally to scrub (⌥ for fine, ⇧ for coarse), or double-click to
/// type an exact number. The first tick of a scrub snapshots for undo; reads
/// top-down each render so live re-eval stays in sync.
struct ScrubField: View {
    let label: String
    let value: Double
    var unit: String = "mm"
    var sensitivity: Double = 0.1
    var minValue: Double = -.greatestFiniteMagnitude
    let onChange: (_ value: Double, _ snapshotFirst: Bool) -> Void
    @State private var base: Double?
    @State private var typing: String?
    @FocusState private var focused: Bool

    var body: some View {
        // Inspector-standard: label, text field, unit, stepper. The label is
        // still a scrub handle (drag it), the way Xcode's and Keynote's are.
        HStack(spacing: 4) {
            if !label.isEmpty {
                Text(label).font(.callout).foregroundStyle(.secondary).lineLimit(1)
                    .contentShape(Rectangle())
                    .onHover { ($0 ? NSCursor.resizeLeftRight : NSCursor.arrow).set() }
                    .gesture(scrub)
                    .help("Drag to scrub (⌥ fine · ⇧ coarse)")
                Spacer(minLength: 2)
            }
            TextField(label.isEmpty ? "Value" : label,
                      text: Binding(get: { typing ?? Self.plain(value) }, set: { typing = $0 }))
                .textFieldStyle(.roundedBorder)
                .font(.callout.monospacedDigit())
                .multilineTextAlignment(.trailing)
                .frame(width: 52)
                .focused($focused)
                // A click must take focus: in a floating toolbar the field
                // could stay unfocused (a triple-click left keystrokes going
                // to the sketch dimension field focused before it).
                // `focused = true` alone is not enough: when SwiftUI's focus
                // state already reads true (stale after another field took the
                // window's field editor) setting it again is a no-op, and the
                // gesture itself eats the mouseDown NSTextField uses to take
                // first responder. So also hand the field editor over the
                // AppKit way, to the text field actually under the pointer.
                .simultaneousGesture(TapGesture().onEnded {
                    focused = true
                    #if os(macOS)
                    DispatchQueue.main.async { Self.takeFirstResponderUnderPointer() }
                    #endif
                })
                .onSubmit { commit() }
                .onEscape { typing = nil; focused = false }
                .onChange(of: focused) { _, now in if !now { commit() } }
            if !unit.isEmpty {
                Text(unit).font(.callout).foregroundStyle(.secondary).lineLimit(1).fixedSize().frame(minWidth: 14, alignment: .leading)
            }
            Stepper(label, value: Binding(get: { value }, set: { onChange(max(minValue, $0), true) }),
                    step: sensitivity * 10)
                .labelsHidden()
        }
        .controlSize(.small)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label.isEmpty ? "Value" : label)
        .accessibilityValue(formatted)
        .accessibilityAdjustableAction { direction in
            let step = sensitivity * 10
            onChange(max(minValue, value + (direction == .increment ? step : -step)), true)
        }
    }

    private var scrub: some Gesture {
        DragGesture(minimumDistance: 2)
            .onChanged { v in
                let first = base == nil
                let b = base ?? value
                if base == nil { base = b }
                // Snap to the stepper's step (1 mm by default; ⌥ 0.1, ⇧ 10) so a
                // mouse-only edit lands on round numbers (it gave 44.70).
                let scale = Self.modifierScale
                let step = sensitivity * 10 * scale
                let raw = b + Double(v.translation.width) * sensitivity * scale
                let snapped = step > 0 ? (raw / step).rounded() * step : raw
                onChange(max(minValue, snapped), first)
            }
            .onEnded { _ in base = nil }
    }

    #if os(macOS)
    /// Make the NSTextField under the mouse first responder in the key window,
    /// unless it already owns the field editor. Returns the field it focused.
    @discardableResult
    static func takeFirstResponderUnderPointer(in window: NSWindow? = NSApp.keyWindow) -> NSTextField? {
        guard let window, let content = window.contentView else { return nil }
        let p = content.superview?.convert(window.mouseLocationOutsideOfEventStream, from: nil)
            ?? window.mouseLocationOutsideOfEventStream
        var v = content.hitTest(p)
        while let view = v, !(view is NSTextField) { v = view.superview }
        guard let field = v as? NSTextField, field.isEditable else { return nil }
        return focus(field)
    }

    /// Move the window's field editor to `field` and select its text.
    @discardableResult
    static func focus(_ field: NSTextField) -> NSTextField? {
        guard let window = field.window else { return nil }
        if field.currentEditor() == nil { window.makeFirstResponder(field) }
        field.currentEditor()?.selectAll(nil)
        return field.currentEditor() != nil ? field : nil
    }
    #endif

    static func plain(_ v: Double) -> String {
        abs(v - v.rounded()) < 0.001 ? String(Int(v.rounded())) : String(format: "%.2f", v)
    }

    /// ⌥ scrubs at a tenth of the rate, ⇧ at ten times — the Xcode/Motion idiom.
    private static var modifierScale: Double {
        #if os(macOS)
        let flags = NSEvent.modifierFlags
        if flags.contains(.option) { return 0.1 }
        if flags.contains(.shift) { return 10 }
        #endif
        return 1
    }

    private func commit() {
        defer { typing = nil }
        guard let t = typing, let v = Self.parse(t) else { return }
        onChange(max(minValue, v), true)
    }

    /// Accept the user's locale decimal separator as well as a plain dot, and
    /// ignore a trailing unit the user may have typed along with the number.
    static func parse(_ text: String) -> Double? {
        var s = text.trimmingCharacters(in: .whitespaces)
        if let firstNonNumber = s.firstIndex(where: { !"0123456789.,-+eE".contains($0) }) {
            s = String(s[..<firstNonNumber]).trimmingCharacters(in: .whitespaces)
        }
        if let v = Double(s) { return v }
        let f = NumberFormatter()
        f.locale = .current
        f.numberStyle = .decimal
        return f.number(from: s)?.doubleValue
    }

    private var formatted: String {
        let s = abs(value - value.rounded()) < 0.001 ? String(Int(value.rounded())) : String(format: "%.2f", value)
        return unit.isEmpty ? s : "\(s) \(unit)"
    }
}

/// Inline rename field for a feature-tree row — commits on Return/blur, cancels
/// on Escape.
struct RenameField: View {
    let initial: String
    let commit: (String) -> Void
    let cancel: () -> Void
    @State private var text: String
    @FocusState private var focused: Bool

    init(initial: String, commit: @escaping (String) -> Void, cancel: @escaping () -> Void) {
        self.initial = initial
        self.commit = commit
        self.cancel = cancel
        _text = State(initialValue: initial)
    }

    var body: some View {
        TextField("", text: $text)
            .textFieldStyle(.plain)
            .font(.callout)
            .focused($focused)
            .onAppear { focused = true }
            .onSubmit { commit(text) }
            .onEscape { cancel() }
            .onChange(of: focused) { _, now in if !now { commit(text) } }
            .padding(.horizontal, Theme.Space.xs).padding(.vertical, 1)
            .controlWell(active: true)
            .accessibilityLabel("Rename")
    }
}

// MARK: - Transports and pills

/// The slice-1 Receipt: the live cross-domain verdict. Drag the connector and
/// the min-wall check flips green→red as the cutout threatens the housing.
struct GripperReceiptPill: View {
    let model: EditorModel
    var body: some View {
        let ok = model.connectorOK
        return HStack(spacing: 11) {
            HStack(spacing: 5) {
                Image(systemName: "bolt.fill")
                Text("connector \(Int(model.connectorX.rounded())) mm")
            }
            .foregroundStyle(.secondary)
            Divider().frame(height: 12)
            HStack(spacing: 5) {
                Image(systemName: ok ? "checkmark.seal.fill" : "exclamationmark.triangle.fill")
                Text(String(format: "min-wall %.1f mm", max(0, model.connectorMinWall)))
            }
            .foregroundStyle(ok ? Color.green : Color.orange)
        }
        .font(.callout.monospacedDigit())
        .padding(.horizontal, 14).padding(.vertical, 8)
        .pillSurface()
        .overlay(Capsule(style: .continuous)
            .strokeBorder((ok ? Color.green : Color.orange).opacity(0.35), lineWidth: 1))
        .animation(Motion.snappy, value: ok)
    }
}

/// Play/pause + scrub transport for kinematic joint playback. Scrubbing
/// pauses (direct control beats a fighting timer); play loops the timeline.
struct PlaybackBar: View {
    let model: EditorModel

    private var duration: Double { model.timeline?.durationS ?? 1 }

    var body: some View {
        HStack(spacing: Theme.Space.m) {
            Button {
                model.togglePlayback()
            } label: {
                Image(systemName: model.isPlaying ? "pause.fill" : "play.fill")
                    .font(.body.weight(.semibold))
                    .frame(width: 22, height: 22)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(model.isPlaying ? "Pause" : "Play")
            .accessibilityLabel(model.isPlaying ? "Pause" : "Play")

            Slider(
                value: Binding(
                    get: { model.playbackTime },
                    set: { model.setPlaybackTime($0) }
                ),
                in: 0...max(duration, 0.001),
                onEditingChanged: { began in
                    if began { model.pausePlayback() }
                }
            )
            .controlSize(.small)
            .frame(width: 220)
            .accessibilityLabel("Timeline position")

            Text(String(format: "%.2f / %.2f s", model.playbackTime, duration))
                .font(.caption.weight(.medium).monospacedDigit())
                .foregroundStyle(.secondary)
                .frame(minWidth: 86, alignment: .trailing)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .pillSurface()
    }
}

// MARK: - Sketch preview overlay (shared by every viewport)

// MARK: - Transform gizmo overlay (shared by every viewport)

/// Gizmo handle ink: X red / Y green / Z blue.
enum GizmoInk {
    static let x = NSColor(srgbRed: 0.95, green: 0.30, blue: 0.34, alpha: 1)
    static let y = NSColor(srgbRed: 0.42, green: 0.80, blue: 0.44, alpha: 1)
    static let z = NSColor(srgbRed: 0.32, green: 0.58, blue: 0.98, alpha: 1)

    static func brighten(_ c: NSColor) -> NSColor {
        let s = c.usingColorSpace(.sRGB) ?? c
        return NSColor(srgbRed: min(1, s.redComponent * 1.2 + 0.18),
                       green: min(1, s.greenComponent * 1.2 + 0.18),
                       blue: min(1, s.blueComponent * 1.2 + 0.18), alpha: 1)
    }
}

/// Scale factor that keeps the gizmo a constant fraction of the view
/// height: target world arm length is proportional to orbit distance, so
/// zooming in/out never changes its apparent size.
@MainActor
func gizmoScreenScale(model: EditorModel) -> Float {
    let armKernel = model.gizmoArmLength()
    let armWorld = armKernel * model.displayScale
    guard armWorld > 1e-6 else { return 1 }
    return (0.2 * model.distance) / armWorld
}

extension Color {
    /// `Color(nsColor:)` / `Color(uiColor:)` under one spelling for shared code.
    init(portedColor c: NSColor) {
        #if os(macOS)
        self.init(nsColor: c)
        #else
        self.init(uiColor: c)
        #endif
    }
}
