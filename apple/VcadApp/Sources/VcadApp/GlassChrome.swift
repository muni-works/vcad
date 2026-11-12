import SwiftUI
import RealityKit
#if os(macOS)
import AppKit
#endif

// The Liquid Glass shell (vcad 1.0): a slim toolbar, floating glass sidebar
// and inspector, one centred dock, and three signature moments — the edit bar
// beside the selection, the history filmstrip, and the agent suggestion pill.
//
// State that belongs to the chrome rather than the document lives in
// `GlassUI.shared`, so the document model does not grow UI flags.

// MARK: - Layout constants

enum Glass {
    /// Sidebar and inspector width.
    static let panelWidth: CGFloat = 170
    /// Even inset from the window edges.
    static let inset: CGFloat = 10
    /// The window's corner radius (macOS 26 document windows).
    static let windowRadius: CGFloat = 26
    /// A panel nested `inset` inside the window keeps the corners concentric.
    static var panelRadius: CGFloat { windowRadius - inset }
    /// Height of the title-bar row itself.
    static let toolbarRow: CGFloat = 52
    /// Air above the row in a window, so the dock doesn't crowd the top edge.
    static let toolbarDrop: CGFloat = 6
    /// Vertical center of the row, which the window controls line up with.
    static var toolbarCenter: CGFloat { toolbarDrop + toolbarRow / 2 }
    /// Space the row takes from the top of the window.
    static var toolbarHeight: CGFloat { toolbarDrop + toolbarRow }
    /// Height of the tool dock's capsule.
    static let dockHeight: CGFloat = 38
    /// Space the dock takes from the bottom, for chrome that stacks above it.
    /// Air between the dock and the window's bottom edge.
    static let dockLift: CGFloat = inset + 6
    static var dockClearance: CGFloat { dockLift + dockHeight + 8 }
    /// Below this window width the dock folds modify and combine tools too,
    /// so it clears the side panels.
    static let dockFullWidth: CGFloat = 1080
}

/// The five workspaces the dock offers. Assembly and Fab are views over the
/// Design viewport (the assembly panel / the fabrication panel); the other
/// three map one-to-one onto `Workspace`.
enum DockSpace: String, CaseIterable, Identifiable {
    case design, assembly, drawing, electronics, fab
    var id: String { rawValue }
    var label: String {
        switch self {
        case .design: "Design"
        case .assembly: "Assembly"
        case .drawing: "Drawing"
        case .electronics: "Electronics"
        case .fab: "Fab"
        }
    }
    var symbol: String {
        switch self {
        case .design: "cube"
        case .assembly: "square.stack.3d.up"
        case .drawing: "ruler"
        case .electronics: "cpu"
        case .fab: "hammer"
        }
    }
    var key: KeyEquivalent {
        switch self {
        case .design: "1"
        case .assembly: "2"
        case .drawing: "3"
        case .electronics: "4"
        case .fab: "5"
        }
    }
    /// "⌃⌘1" — the View-menu shortcut, shown in the workspace picker.
    var shortcutHint: String { "⌃⌘\(String(key.character))" }
}

@MainActor @Observable
final class GlassUI {
    static let shared = GlassUI()

    var space: DockSpace = .design
    var historyOpen = false
    var paletteOpen = false
    var searchOpen = false
    var suggestion: AgentSuggestion?
    /// Wall time of the last viewport geometry build, ms (set by the renderer).
    var lastBuildMillis: Double = 0
    /// Wall time of the last history seek until its rebuild landed, ms.
    var lastSeekMillis: Double?
    @ObservationIgnored var shimmerTask: Task<Void, Never>?
    /// The one floating panel (joint/mate editors, parameters, electronics
    /// tools) — a non-modal glass card under the dock, never a sheet.
    var panel: PanelRequest?

    func present(_ title: String, symbol: String, width: CGFloat = 400,
                 onClose: @escaping () -> Void = {}, @ViewBuilder content: @escaping () -> some View) {
        panel = PanelRequest(title: title, symbol: symbol, width: width, onClose: onClose) { AnyView(content()) }
    }
    func closePanel() {
        let p = panel
        panel = nil
        p?.onClose()
    }

    /// The keyboard cheat sheet (?).
    var shortcutsShown = false
    /// A transient confirmation ("Undo Fillet"), auto-hidden.
    var toast: (text: String, symbol: String)?
    @ObservationIgnored private var toastTask: Task<Void, Never>?

    func flash(_ text: String, symbol: String) {
        toast = (text.replacingOccurrences(of: "Claude: ", with: ""), symbol)
        toastTask?.cancel()
        toastTask = Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(1400))
            guard !Task.isCancelled else { return }
            GlassUI.shared.toast = nil
        }
    }

    /// A bare-letter key from the window: run the current workspace's tool
    /// bound to it. Returns false when nothing is bound (the key beeps on).
    func handleKey(_ ch: Character, model: EditorModel) -> Bool {
        if ch == "?" { shortcutsShown.toggle(); return true }
        if model.sketching {
            let key = Character(ch.lowercased())
            if key == "x" { model.sketch?.toggleConstruction(); model.sketchDirty = true; return true }
            guard let t = SketchToolKind.allCases.first(where: { $0.shortcut == key }) else { return false }
            model.setSketchTool(t)
            return true
        }
        guard model.activeFeatureOp == nil, !model.showsCommandPalette else { return false }
        let key = Character(ch.lowercased())
        guard let t = DockTools.tools(space, model: model).first(where: { $0.shortcut == key }) else { return false }
        guard t.enabled else { flash("\(t.label) — \(t.hint)", symbol: "exclamationmark.circle"); return true }
        t.action()
        return true
    }
    /// Viewport thumbnails per history frame (frame index → image).
    var thumbnails: [Int: NSImage] = [:]

    /// Enter a dock workspace, keeping the document model's workspace and
    /// panels in step with it.
    func enter(_ s: DockSpace, model: EditorModel) {
        space = s
        switch s {
        case .design:
            model.workspace = .design
            model.assembly.showsPanel = false
            FabStore.shared.shown = false
        case .assembly:
            model.workspace = .design
            model.assembly.showsPanel = true
            FabStore.shared.shown = false
        case .drawing:
            model.workspace = .drawing
        case .electronics:
            model.workspace = .electronics
        case .fab:
            model.workspace = .design
            model.assembly.showsPanel = false
            FabStore.shared.shown = true
        }
    }

    /// Re-derive the dock workspace when something else switched it (a menu,
    /// the agent, a document that opens straight into electronics).
    func sync(from model: EditorModel) {
        switch model.workspace {
        case .drawing: space = .drawing
        case .electronics: space = .electronics
        case .manufacture: space = .fab
        case .design:
            if FabStore.shared.shown { space = .fab }
            else if model.assembly.showsPanel { space = .assembly }
            else if space != .design && space != .assembly && space != .fab { space = .design }
            else if space == .assembly || space == .fab { space = .design }
        }
    }
}

// MARK: - Surfaces

/// Glass that degrades to an opaque material under Reduce Transparency.
private struct GlassSurface<S: InsettableShape>: ViewModifier {
    let shape: S
    var tint: Color? = nil
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @Environment(\.colorScheme) private var scheme

    func body(content: Content) -> some View {
        if reduceTransparency {
            content
                .background(Color(nsColor: .windowBackgroundColor), in: shape)
                .overlay(shape.strokeBorder(.separator, lineWidth: 1))
        } else if #available(macOS 26.0, *) {
            content.glassEffect(tint.map { .regular.tint($0) } ?? .regular, in: shape)
        } else {
            content
                .background(.regularMaterial, in: shape)
                .overlay(shape.strokeBorder(.separator, lineWidth: 1))
                .shadow(color: .black.opacity(scheme == .dark ? 0.32 : 0.14), radius: 14, y: 5)
        }
    }
}

extension View {
    /// A floating glass panel nested concentrically inside the window corner.
    func glassPanel() -> some View {
        modifier(GlassSurface(shape: RoundedRectangle(cornerRadius: Glass.panelRadius, style: .continuous)))
    }
    func glassCapsule(tint: Color? = nil) -> some View {
        modifier(GlassSurface(shape: Capsule(style: .continuous), tint: tint))
    }
}

/// Spring used for every chrome morph; Reduce Motion gets a plain fade.
@MainActor
func glassSpring(_ reduceMotion: Bool) -> Animation {
    reduceMotion ? .easeInOut(duration: 0.15) : .spring(response: 0.38, dampingFraction: 0.82)
}

// MARK: - Dock host

/// The combined tool dock, centered along the bottom of the window.
struct DockHost: View {
    @Bindable var model: EditorModel
    @State private var width: CGFloat = 1240
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        // While sketching the dock itself becomes the sketch bar.
        ZStack {
            if model.sketching {
                SketchBar(model: model)
                    .modifier(ChromeRegion(key: "sketchBar"))
                    .transition(reduceMotion ? .opacity : .scale(scale: 0.92).combined(with: .opacity))
            } else {
                GlassDock(model: model, compact: width < Glass.dockFullWidth)
                    .modifier(ChromeRegion(key: "dock"))
                    .transition(reduceMotion ? .opacity : .scale(scale: 0.92).combined(with: .opacity))
            }
        }
            .animation(glassSpring(reduceMotion), value: model.sketching)
            .padding(.bottom, Glass.dockLift)
            .frame(maxWidth: .infinity)
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
    }
}

// MARK: - Toolbar

/// The title-bar row: window controls (system), sidebar toggle, the status
/// field, inspector toggle. It says what's true of the document; the dock at
/// the bottom of the window is how you act on it.
struct GlassToolbar: View {
    @Bindable var model: EditorModel
    @State private var connectionShown = false
    @State private var toolbarWidth: CGFloat = 1240
    @State private var progressShown = false
    @State private var hoveringStatus = false
    @State private var detailsShown = false

    var body: some View {
        HStack(spacing: Theme.Space.m) {
            iconButton("sidebar.leading", "Show or hide the sidebar", active: model.showsTree) {
                withAnimation(Motion.panel) { model.showsTree.toggle() }
            }
            Spacer(minLength: Theme.Space.m)
            if model.workspace == .manufacture { machineChip }
            exportMenu
            agentsButton
            // Release to Desktop / Open in Window (⇧⌘D), out of the View menu
            // and next to the panel toggles where it's discoverable.
            iconButton(model.isWindowed ? "arrow.up.forward.app" : "macwindow",
                       model.isWindowed ? "Release to Desktop (⇧⌘D)" : "Open in Window (⇧⌘D)",
                       active: false) {
                ReleaseWindowController.shared.setWindowed(!model.isWindowed)
            }
            iconButton("sidebar.trailing", "Show or hide the inspector", active: model.showsInspector) {
                withAnimation(Motion.panel) { model.showsInspector.toggle() }
            }
        }
        .padding(.leading, model.isWindowed ? 84 : Glass.inset + 6)
        .padding(.trailing, Glass.inset + 6)
        .frame(height: Glass.toolbarRow)
        // On the window's center, whatever sits either side of it.
        .overlay { statusField.modifier(ChromeRegion(key: "statusField")) }
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { toolbarWidth = $0 }
        .padding(.top, model.isWindowed ? Glass.toolbarDrop : 0)
        .background {
            // ⌘F and ⌘K both open the command palette.
            Group {
                Button("") { beginSearch() }.keyboardShortcut("f", modifiers: .command)
                Button("") { beginSearch() }.keyboardShortcut("k", modifiers: .command)
            }.frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
        }
    }

    /// Shrinks with the window, and is the same size in either state.
    private var fieldWidth: CGFloat { min(340, max(200, toolbarWidth * 0.3)) }

    /// The second line when no task is running, only when there's something
    /// to say. Running work comes from `model.progress` (ProgressCenter.swift).
    private func news(at now: Date) -> String? {
        // An agent over the live link: it has written in the last few seconds.
        if let at = model.agentLinkActiveAt, now.timeIntervalSince(at) < 4 { return "Agent working" }
        if let line = model.sketchStatusLine(at: now) { return line }
        return model.documentDirty ? "Edited" : nil
    }

    /// When `news(at:)` changes with nothing but the clock moving.
    private func newsExpiries(after now: Date) -> [Date] {
        [model.agentLinkActiveAt.map { $0.addingTimeInterval(4) }, model.sketchBlockerShownUntil]
            .compactMap { $0 }.filter { $0 > now }.sorted()
    }

    /// News expiries, plus a tick a second while a task runs (its ETA).
    private func statusTicks(after now: Date) -> [Date] {
        guard shownTask != nil else { return newsExpiries(after: now) }
        return (1...600).map { now.addingTimeInterval(Double($0)) }
    }

    /// The task the pill shows, once it has run long enough to be worth
    /// showing (a cache-hit open or a one-step solve never flashes).
    private var shownTask: ProgressTask? { progressShown ? model.progress.primary : nil }

    /// A failed or warned open: stays in the pill until dismissed.
    private var loadOutcome: LoadProgress? {
        guard let l = model.loading, !l.isRunning else { return nil }
        return l
    }

    /// The document's name at rest; a click, ⌘F or ⌘K opens the command palette.
    private var statusField: some View {
        Group {
                Button { statusClicked() } label: {
                    // Ticks only at the moments the line can expire on its own
                    // (a linked agent going quiet, a sketch blocker timing out)
                    // and once a second while a task runs (ETA); everything
                    // else re-renders through observation. A free-running
                    // clock at rest kept the window compositing.
                    TimelineView(.explicit(statusTicks(after: .now))) { context in
                        let line = statusLine(at: context.date)
                        VStack(spacing: 0) {
                            Text(pillTitle).font(.system(size: 12, weight: .medium))
                                .lineLimit(1).truncationMode(.middle)
                            if let line {
                                Text(line.text).font(.system(size: 10))
                                    .foregroundStyle(line.tint.map { AnyShapeStyle($0) } ?? AnyShapeStyle(.secondary))
                                    .lineLimit(1).truncationMode(.middle)
                                    // Steady digits: monospaced, no rolling transition.
                                    .monospacedDigit()
                                    .contentTransition(.identity)
                            }
                        }
                        .padding(.horizontal, trailingControl ? 14 : 0)
                        .frame(maxWidth: .infinity, maxHeight: .infinity).contentShape(Capsule())
                        .animation(Motion.smooth, value: line?.text == nil)
                    }
                }
                .buttonStyle(.plain)
                .help(statusHelp)
                .accessibilityLabel([pillTitle, statusLine(at: .now)?.text].compactMap { $0 }.joined(separator: ", "))
                .accessibilityHint(shownTask != nil || loadOutcome != nil ? "Show details" : "Opens the command palette")
        }
        .padding(.horizontal, 14)
        .frame(width: fieldWidth, height: 32)
        .overlay(alignment: .trailing) {
            trailingButton.padding(.trailing, 8)
        }
        .overlay(alignment: .bottom) {
            if let task = shownTask {
                PillProgressBar(fraction: task.fraction)
                    .padding(.horizontal, 16).padding(.bottom, 1)
            }
        }
        .onHover { hoveringStatus = $0 }
        .popover(isPresented: $detailsShown, arrowEdge: .bottom) {
            ProgressDetails(model: model) { detailsShown = false }
        }
        .task(id: model.progress.primary?.id) {
            // Show a task only once it has run for a beat.
            guard model.progress.primary != nil else { progressShown = false; return }
            if progressShown { return }
            try? await Task.sleep(for: .milliseconds(250))
            withAnimation(Motion.smooth) { progressShown = model.progress.primary != nil }
        }
        .onChange(of: model.progress.tasks.count) { _, n in
            // The list is only for more than one task, and closes with nothing to show.
            if n == 0 && loadOutcome == nil { detailsShown = false }
        }
        .reportsProgress(model.progress, id: "agent", title: "Agent working",
                         active: AgentSession.shared.running,
                         cancel: { AgentSession.shared.stop() })
        .glassCapsule()
    }

    /// The document's name — or, while a new file opens, that file's.
    private var pillTitle: String {
        if let l = model.loading { return l.url.deletingPathExtension().lastPathComponent }
        return model.source.label
    }

    /// The second line: the running task, a failed / warned open, or news.
    private func statusLine(at now: Date) -> (text: String, tint: Color?)? {
        if let task = shownTask {
            let extra = model.progress.tasks.count - 1
            return (task.line + (extra > 0 ? " · +\(extra)" : ""), nil)
        }
        if let outcome = loadOutcome {
            return (outcome.stageLabel, outcome.isFailed ? .red : .orange)
        }
        return news(at: now).map { ($0, nil) }
    }

    private var statusHelp: String {
        if let t = shownTask {
            let detail = [t.stage, t.detailLine(now: .now)].compactMap { $0 }.joined(separator: " · ")
            return [t.title, detail.isEmpty ? nil : detail, t.cancellable ? "hover for ✕ to cancel" : nil]
                .compactMap { $0 }.joined(separator: " — ")
        }
        if loadOutcome != nil { return "Click for details" }
        return "\(model.source.label) — click to search commands (⌘K)"
    }

    /// Running work or an open's outcome: the pill shows details. At rest it searches.
    private func statusClicked() {
        if (shownTask != nil && model.progress.tasks.count > 1) || loadOutcome != nil {
            detailsShown.toggle()
        } else {
            beginSearch()
        }
    }

    /// The trailing ✕ (cancel the headlined task) appears on hover.
    private var trailingControl: Bool {
        hoveringStatus && (shownTask?.cancellable == true)
    }

    @ViewBuilder private var trailingButton: some View {
        if trailingControl, let task = shownTask {
            Button { model.progress.cancel(task.id) } label: {
                Image(systemName: "xmark.circle.fill").font(.system(size: 13))
                    .symbolRenderingMode(.hierarchical).foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .help("Cancel \(task.title.lowercased())")
            .accessibilityLabel("Cancel \(task.title)")
            .transition(.opacity)
        }
    }

    /// One search for the whole app: the command palette.
    /// One search for the whole app: the command palette. A second click
    /// closes it, like Spotlight.
    private func beginSearch() {
        model.showsCommandPalette.toggle()
    }

    private var machineChip: some View {
        Button { connectionShown.toggle() } label: {
            HStack(spacing: 6) {
                Image(systemName: model.cnc.machine.connected ? "checkmark.circle.fill" : "circle.dashed")
                Text(model.cnc.machine.connected ? model.cnc.machine.status.state : "Disconnected")
            }.font(.caption)
        }
        .buttonStyle(.borderless)
        .help("Machine connection")
        .accessibilityLabel("Machine connection, \(model.cnc.machine.summary)")
        .popover(isPresented: $connectionShown, arrowEdge: .bottom) {
            CNCConnectionView(cnc: model.cnc).padding(18).frame(width: 320)
        }
    }

    /// Everything that gets the model out of vcad, in one place: the File ▸
    /// Export, Export Drawing and Render items plus the print workspace.
    private var exportMenu: some View {
        Menu {
            Button("STL…") { exportDocumentPanel(model, format: .stl) }.disabled(!model.canExport)
            Button("STEP…") { exportDocumentPanel(model, format: .step) }.disabled(!model.canExportDocument)
            Button("GLB…") { exportDocumentPanel(model, format: .glb) }.disabled(!model.canExportDocument)
            Button("USDZ…") { exportUSDZPanel(model) }.disabled(!model.canExport)
            Divider()
            Button("Drawing as PDF…") { DrawingStore.shared.model = model; DrawingStore.shared.export(.pdf) }
                .disabled(!model.canExportDocument)
            Button("Drawing as DXF…") { DrawingStore.shared.model = model; DrawingStore.shared.export(.dxf) }
                .disabled(!model.canExportDocument)
            Divider()
            Button("3D Print…") { withAnimation(Motion.panel) { model.workspace = .manufacture } }
                .disabled(!model.canExport)
            Button("Render Photoreal…") { PhotorealRenderController.shared.show(for: model) }
                .disabled(!model.canExportDocument)
        } label: {
            Image(systemName: "square.and.arrow.up").font(.system(size: 15, weight: .regular))
                .frame(width: 30, height: 26).contentShape(Rectangle())
                .foregroundStyle(.secondary)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Export, print or render")
        .accessibilityLabel("Export, print or render")
    }

    /// The local agent: lit while one is working in this document, and the
    /// way in to connecting Claude, Codex or Cursor.
    private var agentsButton: some View {
        // Ticks only when the "working" glow can expire on its own; a
        // free-running clock would keep the idle window compositing.
        TimelineView(.explicit([model.agentLinkActiveAt.map { $0.addingTimeInterval(4) }]
            .compactMap { $0 }.filter { $0 > .now })) { context in
            let working = AgentSession.shared.running
                || (model.agentLinkActiveAt.map { context.date.timeIntervalSince($0) < 4 } ?? false)
            iconButton(working ? "sparkles" : "sparkle",
                       working ? "Agent working — connect agents…" : "Connect agents…",
                       active: working) { AgentInstallPrompt.run() }
        }
    }

    private func iconButton(_ symbol: String, _ help: String, active: Bool, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol).font(.system(size: 15, weight: .regular))
                .frame(width: 30, height: 26).contentShape(Rectangle())
                .foregroundStyle(active ? .primary : .secondary)
        }
        .buttonStyle(.borderless)
        .help(help)
        .accessibilityLabel(help)
        .accessibilityAddTraits(active ? .isSelected : [])
    }
}

// MARK: - Dock tools

/// Each workspace's dock tools. Every workspace has the same structure: a
/// short row of verbs, the active one tinted.
@MainActor
enum DockTools {
    static func tools(_ space: DockSpace, model m: EditorModel) -> [Tool] {
        switch space {
        case .design: return design(m)
        case .assembly: return assembly(m)
        case .drawing: return drawing(m)
        case .electronics: return electronics(m)
        case .fab: return fab(m)
        }
    }

    private static func design(_ m: EditorModel) -> [Tool] {
        // The Untitled sandbox isn't a document yet; the first modeling action
        // promotes it (same shape, real features) instead of disabling the tools.
        let sandbox = m.source.isSandbox && m.documentJSON == nil
        let doc = m.usesDocumentTree
        let selected = m.selectedPartIndex != nil || m.picking.selectionPart != nil
        var out: [Tool] = []
        out.append(Tool(id: "sketch", label: "Sketch", symbol: "scribble.variable", isActive: m.sketching,
                        enabled: doc || sandbox, hint: doc || sandbox ? "" : "Open a document") {
            if m.promoteSandboxToDocument(blank: true) { m.enterSketch() }
        })
        out += m.tools(for: .create).filter { $0.id.hasPrefix("shape.") }.map { t in
            var t = t; t.group = "Primitives"; return t
        }
        for op in FeatureOp.modifyOps where [.fillet, .chamfer, .shell, .hole, .circularPattern].contains(op) {
            out.append(Tool(id: "op.\(op.rawValue)", label: op.label, symbol: op.symbol,
                            isActive: m.activeFeatureOp == op, enabled: sandbox || (doc && selected),
                            hint: sandbox || selected ? "" : "Select a part first", compactGroup: "Modify") {
                if m.promoteSandboxToDocument() { m.activeFeatureOp = op }
            })
        }
        let ready = m.multiSelectedParts.count == 2
        // Union, Subtract and Intersect read the same: selected part is the
        // target, the next click is the tool.
        for op in [BooleanOp.union, .difference, .intersection] {
            let waiting = m.pendingBoolean?.op == op
            let prompt = op == .union ? "click the part to join"
                : op == .difference ? "click the part to cut away" : "click the part to intersect with"
            out.append(Tool(id: "bool.\(op.rawValue)", label: waiting ? "\(op.label) — \(prompt)" : op.label,
                            symbol: op.symbol, isActive: waiting, enabled: doc && (ready || selected || waiting),
                            hint: ready || selected ? "" : "Select the first part", compactGroup: "Combine") {
                if waiting { m.pendingBoolean = nil } else { m.beginCombine(op) }
            })
        }
        out.append(Tool(id: "measure", label: "Measure", symbol: "ruler", isActive: m.measuring, enabled: doc) {
            m.measuring.toggle()
        })
        out.append(Tool(id: "mallet", label: "Mallet", symbol: "hammer", isActive: SoundStore.shared.malletArmed, enabled: doc) {
            SoundStore.shared.malletArmed.toggle()
        })
        return keyed(out, ["sketch": "s", "shape.cube": "b", "shape.cylinder": "y", "op.fillet": "f",
                           "op.chamfer": "c", "op.shell": "k", "op.hole": "h", "op.circularPattern": "p",
                           "bool.difference": "x", "bool.union": "u", "bool.intersection": "n", "measure": "i", "mallet": "m",
                           "shape.torus": "t", "shape.wedge": "w", "shape.prism": "r"])
    }

    /// Attach bare-letter shortcuts by tool id.
    private static func keyed(_ tools: [Tool], _ keys: [String: Character]) -> [Tool] {
        tools.map { t in var t = t; t.shortcut = keys[t.id]; return t }
    }

    private static func assembly(_ m: EditorModel) -> [Tool] {
        let has = m.assemblyInstanceCount > 0
        return keyed([
            Tool(id: "asm.insert", label: "Insert Part", symbol: "plus.square.on.square",
                 isActive: false, enabled: m.selectedPartIndex != nil, hint: "Select a part to instance") {
                if let pi = m.selectedPartIndex { _ = m.insertInstance(ofPart: pi) }
            },
            Tool(id: "asm.panel", label: "Joints & Mates", symbol: "link", isActive: m.assembly.showsPanel) {
                m.assembly.showsPanel.toggle()
            },
            // Was "Check Mates", enabled only once a mate existed — so with six
            // instances and no mates the dock's check stayed grey and
            // interference was only reachable from the Analyze menu.
            Tool(id: "asm.check", label: "Check Interference", symbol: "checkmark.seal",
                 isActive: AnalysisStore.shared.shown && AnalysisStore.shared.tab == .clearance,
                 enabled: m.clearanceBodyCount >= 2, hint: "Needs two or more parts") {
                m.checkInterference()
            },
            Tool(id: "asm.explode", label: "Explode", symbol: "arrow.up.left.and.arrow.down.right",
                 isActive: m.explode > 0, enabled: has) {
                withAnimation(Motion.smooth) { m.explode = m.explode > 0 ? 0 : 1 }
            },
            Tool(id: "asm.timeline", label: "Timeline", symbol: "timeline.selection",
                 isActive: m.assembly.showsTimeline, enabled: has) {
                m.assembly.showsTimeline.toggle()
            },
        ], ["asm.insert": "n", "asm.panel": "j", "asm.check": "k", "asm.explode": "e", "asm.timeline": "t"])
    }

    /// Drawing: exclusive modes only. On/off display toggles live in the
    /// inspector and the View menu, so the dock never shows three "active".
    private static func drawing(_ m: EditorModel) -> [Tool] {
        let s = DrawingStore.shared
        return keyed([
            Tool(id: "dwg.select", label: "Select", symbol: "cursorarrow", isActive: s.tool == .select) { s.tool = .select },
            Tool(id: "dwg.dimension", label: "Dimension", symbol: "ruler", isActive: s.tool == .dimension) { s.tool = .dimension },
            Tool(id: "dwg.centerline", label: "Center Line", symbol: "line.diagonal", isActive: s.tool == .centerLine) { s.tool = .centerLine },
            Tool(id: "dwg.section", label: "Section", symbol: "rectangle.split.2x1", isActive: s.tool == .section,
                 enabled: s.view.canSection, hint: "Not in the isometric view") { s.tool = .section },
            Tool(id: "dwg.detail", label: "Detail", symbol: "plus.magnifyingglass", isActive: s.tool == .detail) { s.tool = .detail },
            Tool(id: "dwg.datum", label: "Datum", symbol: "a.square", isActive: s.tool == .gdt(.datum)) { s.tool = .gdt(.datum) },
            Tool(id: "dwg.frame", label: "Feature Control Frame", symbol: "square.grid.3x1.below.line.grid.1x2",
                 isActive: { if case .gdt(let k) = s.tool { return k != .datum }; return false }()) { s.tool = .gdt(.position) },
        ], ["dwg.select": "v", "dwg.dimension": "m", "dwg.centerline": "l", "dwg.section": "t", "dwg.detail": "d", "dwg.datum": "a", "dwg.frame": "g"])
    }

    /// Electronics: the editing modes plus the two board-wide verbs. An empty
    /// document gets the one thing it can do — start a board.
    private static func electronics(_ m: EditorModel) -> [Tool] {
        let ec = m.electronics
        let t = ec.tools
        guard !ec.board(m).isEmpty else {
            return [
                Tool(id: "ec.create", label: "Create Board", symbol: "plus.rectangle.on.rectangle", isActive: false) { ec.createBoard(m) },
                Tool(id: "ec.import", label: "Import KiCad Board", symbol: "square.and.arrow.down", isActive: false) { t.importBoard(ec, m) },
            ]
        }
        func mode(_ tool: ElectronicsTool, _ label: String, _ symbol: String, boardOnly: Bool = false) -> Tool {
            Tool(id: "ec.\(tool.rawValue)", label: label, symbol: symbol, isActive: ec.tool == tool) {
                if boardOnly && ec.view != .board { ec.view = .board }
                ec.tool = tool
            }
        }
        // Schematic gets schematic verbs; Via / Autoroute / DRC are board work.
        if ec.view == .schematic {
            let empty = ecRows(ec.schematic(m)["components"]).isEmpty
            return keyed([
                mode(.select, "Select", "cursorarrow"),
                mode(.place, "Place Component", "square.on.square.dashed"),
                mode(.wire, "Wire", "point.3.connected.trianglepath.dotted"),
                mode(.label, "Net Label", "tag"),
                empty
                    ? Tool(id: "ec.buildsch", label: "Build Schematic from Board", symbol: "arrow.triangle.branch",
                           isActive: false) { ec.buildSchematicFromBoard(m) }
                    : Tool(id: "ec.check", label: "Check Connectivity", symbol: "checkmark.seal", isActive: ec.checking) { ec.check(m) },
                Tool(id: "ec.erc", label: "Electrical Rule Check", symbol: "bolt.badge.checkmark", isActive: t.sheet == .erc, enabled: !empty, hint: "Build or draw a schematic first") { t.runErc(ec, m) },
                Tool(id: "ec.annotate", label: "Annotate", symbol: "textformat.123", isActive: false) { ec.annotate(m) },
                Tool(id: "ec.tidy", label: "Tidy Sheet", symbol: "square.grid.3x3", isActive: false) { ec.tidySchematic(m) },
                Tool(id: "ec.simulate", label: "Simulate", symbol: "waveform", isActive: t.sheet == .simulate,
                     enabled: !empty, hint: "Build or draw a schematic first") { t.sheet = .simulate },
            ], ["ec.select": "e", "ec.place": "p", "ec.wire": "w", "ec.label": "l", "ec.check": "c", "ec.simulate": "s"])
        }
        return keyed([
            mode(.select, "Select", "cursorarrow"),
            mode(.place, "Place Component", "square.on.square.dashed"),
            mode(.route, "Route", "point.topleft.down.to.point.bottomright.curvepath", boardOnly: true),
            mode(.via, "Via", "smallcircle.filled.circle", boardOnly: true),
            mode(.zone, "Zone", "square.dashed.inset.filled", boardOnly: true),
            Tool(id: "ec.autoroute", label: "Autoroute", symbol: "wand.and.stars", isActive: t.busy == "Autoroute",
                 enabled: t.busy == nil, hint: "Busy") { t.autoroute(ec, m) },
            Tool(id: "ec.drc", label: "DRC", symbol: "checklist", isActive: t.sheet == .drc,
                 enabled: t.busy == nil, hint: "Busy") { t.runDrc(ec, m) },
        ], ["ec.select": "e", "ec.place": "p", "ec.route": "x", "ec.wire": "w", "ec.via": "v",
            "ec.autoroute": "a", "ec.drc": "d"])
    }

    /// The rest of a workspace's verbs, for the ··· palette only.
    static func extras(_ space: DockSpace, model m: EditorModel) -> [Tool] {
        switch space {
        case .electronics:
            let ec = m.electronics, t = ec.tools
            guard !ec.board(m).isEmpty else { return [] }
            return [
                Tool(id: "ec.pair", label: "Diff Pair", symbol: "equal", isActive: false) { t.openLengths(ec, m) },
                Tool(id: "ec.tune", label: "Tune Lengths", symbol: "waveform.path", isActive: false,
                     enabled: !t.tuneNets.isEmpty, hint: "Pick nets in Diff Pair first") { t.tune(ec, m) },
                Tool(id: "ec.fix", label: "Fix DRC", symbol: "wrench.adjustable", isActive: false) { t.fixDrc(ec, m) },
                Tool(id: "ec.stack", label: "Layer Stack", symbol: "square.3.layers.3d", isActive: false) { t.sheet = .stackup },
                Tool(id: "ec.netclass", label: "Net Classes", symbol: "list.bullet.rectangle", isActive: false) { t.sheet = .netClasses },
                Tool(id: "ec.enclose", label: "Enclose Board", symbol: "shippingbox", isActive: false) { t.encloseBoard(ec, m) },
                Tool(id: "ec.fit", label: "Fit Check", symbol: "cube.transparent", isActive: false) { t.sheet = .enclosure },
                Tool(id: "ec.sim", label: "Simulate", symbol: "bolt.horizontal", isActive: false) { t.sheet = .simulate },
                Tool(id: "ec.check", label: "Connectivity", symbol: "link", isActive: false) { ec.check(m) },
                Tool(id: "ec.gerber", label: "Export Gerbers", symbol: "square.and.arrow.up", isActive: false) { t.exportGerbers(ec, m) },
                Tool(id: "ec.import2", label: "Import Board", symbol: "square.and.arrow.down", isActive: false) { t.importBoard(ec, m) },
            ]
        case .drawing:
            let s = DrawingStore.shared
            return [
                Tool(id: "dwg.dxf", label: "Export DXF", symbol: "square.and.arrow.up", isActive: false) { s.export(.dxf) },
                Tool(id: "dwg.pdf", label: "Export PDF", symbol: "doc.richtext", isActive: false) { s.export(.pdf) },
            ]
        default:
            return []
        }
    }

    private static func fab(_ m: EditorModel) -> [Tool] {
        let f = FabStore.shared
        let a = AnalysisStore.shared
        return keyed([
            Tool(id: "fab.sheet", label: "Sheet Metal", symbol: "square.3.layers.3d.down.right",
                 isActive: f.shown && f.tab == .sheet) { a.shown = false; f.shown = true; f.tab = .sheet },
            Tool(id: "fab.print", label: "3D Print", symbol: "printer", isActive: f.shown && f.tab == .print) {
                a.shown = false; f.shown = true; f.tab = .print
            },
            Tool(id: "fab.cam", label: "CAM", symbol: "gearshape.2", isActive: f.shown && f.tab == .cam) {
                a.shown = false; f.shown = true; f.tab = .cam
            },
            Tool(id: "fab.dfm", label: "DFM", symbol: "checkmark.shield", isActive: a.shown && a.tab == .dfm,
                 enabled: m.usesDocumentTree) { a.shown = true; a.tab = .dfm },
            Tool(id: "fab.fea", label: "Stress", symbol: "waveform.path.ecg", isActive: a.shown && a.tab == .fea,
                 enabled: m.usesDocumentTree) { a.shown = true; a.tab = .fea },
        ], ["fab.sheet": "m", "fab.print": "p", "fab.cam": "n", "fab.dfm": "d", "fab.fea": "r"])
    }
}

// MARK: - Dock

/// A dock slot: a single tool, or a family folded behind one button.
enum DockItem: Identifiable {
    case tool(Tool)
    case group(String, [Tool])
    var id: String {
        switch self {
        case .tool(let t): t.id
        case .group(let name, _): "group.\(name)"
        }
    }

    /// Fold tools by `group`, at the position of the family's first member.
    /// `compact` also folds by `compactGroup`.
    static func items(_ tools: [Tool], compact: Bool) -> [DockItem] {
        var out: [DockItem] = []
        var index: [String: Int] = [:]
        for t in tools {
            guard let g = t.group ?? (compact ? t.compactGroup : nil) else { out.append(.tool(t)); continue }
            if let i = index[g], case .group(let name, let members) = out[i] {
                out[i] = .group(name, members + [t])
            } else {
                index[g] = out.count
                out.append(.group(g, [t]))
            }
        }
        return out
    }
}

/// The five workspaces, listed in the popover above the dock's workspace
/// button. Each row shows its ⌃⌘ shortcut (the shortcuts themselves live in
/// the View menu, so they work with the popover closed); Return/Space on a
/// focused row and the shortcuts inside the open popover both switch.
struct WorkspacePicker: View {
    var current: DockSpace
    var choose: (DockSpace) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(DockSpace.allCases) { s in
                Button { choose(s) } label: {
                    HStack(spacing: 8) {
                        Image(systemName: s.symbol).frame(width: 18)
                        Text(s.label)
                        Spacer(minLength: 16)
                        Text(s.shortcutHint).foregroundStyle(.secondary).monospacedDigit()
                        Image(systemName: "checkmark").font(.caption.weight(.semibold))
                            .opacity(s == current ? 1 : 0)
                    }
                    .padding(.horizontal, 10).padding(.vertical, 5)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .keyboardShortcut(s.key, modifiers: [.command, .control])
                .accessibilityLabel(s.label)
                .accessibilityHint("Switch workspace, Control-Command-\(String(s.key.character))")
                .accessibilityAddTraits(s == current ? .isSelected : [])
            }
        }
        .padding(6)
        .frame(minWidth: 200)
    }
}

/// One stable centred capsule: workspace menu | tools | history, palette.
struct GlassDock: View {
    @Bindable var model: EditorModel
    /// Too narrow for every button: fold modify and combine as well.
    var compact = false
    @Bindable var ui = GlassUI.shared
    @State private var openGroup: String?
    @State private var workspacePickerOpen = false
    @Namespace private var morph
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        HStack(spacing: 4) {
            workspaceMenu
            divider
            HStack(spacing: 2) {
                ForEach(DockItem.items(DockTools.tools(ui.space, model: model), compact: compact)) { item in
                    switch item {
                    case .tool(let t): toolButton(t)
                    case .group(let name, let tools): groupButton(name, tools)
                    }
                }
            }
            .id(ui.space)
            .transition(reduceMotion ? .opacity : .asymmetric(
                insertion: .scale(scale: 0.86).combined(with: .opacity),
                removal: .scale(scale: 1.08).combined(with: .opacity)))
            divider
            dockButton("clock.arrow.circlepath", "History", active: ui.historyOpen) {
                withAnimation(glassSpring(reduceMotion)) { ui.historyOpen.toggle() }
            }
            .disabled(!model.usesDocumentTree)
            dockButton("ellipsis", "All tools", active: ui.paletteOpen) { ui.paletteOpen.toggle() }
                .popover(isPresented: $ui.paletteOpen, arrowEdge: .top) { ToolPalette(model: model) }
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 6).padding(.vertical, 3)
        .glassCapsule()
        .animation(glassSpring(reduceMotion), value: ui.space)
        .fixedSize()
        .disabled(model.cnc.machine.active)
        .onChange(of: model.showsParameterTable) { _, on in
            guard on else { if ui.panel?.title == "Parameters" { ui.panel = nil }; return }
            ui.present("Parameters", symbol: "slider.horizontal.3", width: 420,
                       onClose: { model.showsParameterTable = false }) { ParameterTableSheet(model: model) }
        }
        .background {
            Button("") { model.disarm() }.keyboardShortcut(.cancelAction)
                .frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
                .disabled(model.armedShape == nil)
        }
        .onChange(of: model.workspace, initial: true) { _, _ in ui.sync(from: model) }
    }

    private var divider: some View { Divider().frame(height: 22).padding(.horizontal, 4) }

    /// "Fillet  F" — or why it is unavailable.
    static func tooltip(_ t: Tool) -> String {
        let key = t.shortcut.map { "  \(String($0).uppercased())" } ?? ""
        return t.enabled || t.hint.isEmpty ? t.label + key : "\(t.label)\(key) — \(t.hint)"
    }

    /// The dock sits at the bottom of the window, so the workspace picker
    /// opens upward. (A `Menu` always drops down — below the window edge.)
    static let workspacePickerEdge: Edge = .top

    private var workspaceMenu: some View {
        Button { workspacePickerOpen.toggle() } label: {
            HStack(spacing: 4) {
                Text(ui.space.label).font(.callout.weight(.semibold))
                Image(systemName: "chevron.up").font(.caption2.weight(.semibold)).foregroundStyle(.secondary)
            }
            .padding(.horizontal, 10).frame(height: 32).contentShape(Capsule())
        }
        .fixedSize()
        .help("Workspace (⌃⌘1–5)")
        .accessibilityLabel("Workspace, \(ui.space.label)")
        .accessibilityHint("Shows the five workspaces")
        .popover(isPresented: $workspacePickerOpen, arrowEdge: Self.workspacePickerEdge) {
            WorkspacePicker(current: ui.space) { s in
                workspacePickerOpen = false
                ui.enter(s, model: model)
            }
        }
    }

    @ViewBuilder private func toolButton(_ tool: Tool) -> some View {
        Button { if tool.enabled { tool.action() } } label: {
            Image(systemName: tool.symbol).font(.system(size: 16, weight: .medium))
                .frame(width: 38, height: 32)
                .background {
                    if tool.isActive {
                        // Gray, not blue: blue/orange belong to the 3D selection.
                        Capsule(style: .continuous).fill(Color.primary.opacity(0.14))
                            .matchedGeometryEffect(id: "active", in: morph)
                    }
                }
                .foregroundStyle(.primary)
                .contentShape(Capsule())
        }
        .opacity(tool.enabled ? 1 : 0.35)
        .disabled(!tool.enabled)
        .help(Self.tooltip(tool))
        .accessibilityLabel(tool.label)
        .accessibilityAddTraits(tool.isActive ? .isSelected : [])
    }

    /// One button for a family of tools; the popover keeps each one click away.
    @ViewBuilder private func groupButton(_ name: String, _ tools: [Tool]) -> some View {
        let active = tools.first(where: \.isActive)
        Button { openGroup = openGroup == name ? nil : name } label: {
            ZStack(alignment: .bottomTrailing) {
                Image(systemName: active?.symbol ?? tools[0].symbol).font(.system(size: 16, weight: .medium))
                    .frame(width: 38, height: 32)
                Image(systemName: "chevron.down").font(.system(size: 7, weight: .bold))
                    .foregroundStyle(.secondary).offset(x: -3, y: -3)
            }
            .background {
                if active != nil { Capsule(style: .continuous).fill(Color.primary.opacity(0.14)) }
            }
            .contentShape(Capsule())
        }
        .help(name)
        .accessibilityLabel(name)
        .popover(isPresented: Binding(get: { openGroup == name }, set: { if !$0 { openGroup = nil } }),
                 arrowEdge: .top) {
            LazyVGrid(columns: Array(repeating: GridItem(.fixed(72), spacing: 4), count: 4), spacing: 4) {
                ForEach(tools) { t in
                    Button { openGroup = nil; if t.enabled { t.action() } } label: {
                        VStack(spacing: 3) {
                            Image(systemName: t.symbol).font(.system(size: 17))
                            Text(t.label).font(.caption).lineLimit(1)
                        }.frame(width: 72, height: 52).contentShape(Rectangle())
                    }
                    .buttonStyle(.plain).disabled(!t.enabled).opacity(t.enabled ? 1 : 0.35)
                    .help(Self.tooltip(t)).accessibilityLabel(t.label)
                }
            }.padding(Theme.Space.s)
        }
    }

    private func dockButton(_ symbol: String, _ label: String, active: Bool, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol).font(.system(size: 15, weight: .medium))
                .frame(width: 34, height: 32)
                .background(active ? AnyShapeStyle(.quaternary) : AnyShapeStyle(.clear), in: Capsule(style: .continuous))
                .contentShape(Capsule())
        }
        .help(label).accessibilityLabel(label)
        .accessibilityAddTraits(active ? .isSelected : [])
    }
}

/// The ··· palette: every workspace's tools, tabbed.
struct ToolPalette: View {
    @Bindable var model: EditorModel
    @State private var tab: DockSpace = GlassUI.shared.space

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            Picker("Workspace", selection: $tab) {
                ForEach(DockSpace.allCases) { Text($0.label).tag($0) }
            }.pickerStyle(.segmented).labelsHidden()
            let tools = (tab == .design ? designAll : DockTools.tools(tab, model: model)) + DockTools.extras(tab, model: model)
            LazyVGrid(columns: Array(repeating: GridItem(.fixed(84), spacing: 6), count: 4), spacing: 6) {
                ForEach(tools) { t in
                    Button {
                        GlassUI.shared.enter(tab, model: model); t.action(); GlassUI.shared.paletteOpen = false
                    } label: {
                        VStack(spacing: 4) {
                            Image(systemName: t.symbol).font(.system(size: 17))
                            Text(t.label).font(.caption).lineLimit(1)
                        }.frame(width: 84, height: 56).contentShape(Rectangle())
                    }
                    .buttonStyle(.plain).disabled(!t.enabled).opacity(t.enabled ? 1 : 0.35)
                    .help(t.hint.isEmpty ? t.label : "\(t.label) — \(t.hint)")
                    .accessibilityLabel(t.label)
                }
            }
        }.padding(Theme.Space.l).frame(width: 380)
    }

    /// Design in full: every create/modify/combine tool the model offers.
    private var designAll: [Tool] {
        ToolTab.allCases.flatMap { model.tools(for: $0) }
    }
}

// MARK: - Edit bar (signature moment 1)

/// The primary editable parameter of a feature, for the edit bar.
struct EditBarParam {
    let label: String
    let key: String
    let axis: String?
    let unit: String
    let min: Double
    /// One detent (haptic tick) per this many units.
    let detent: Double

    static func of(_ opType: String) -> EditBarParam? {
        switch opType {
        case "Fillet": EditBarParam(label: "Radius", key: "radius", axis: nil, unit: "mm", min: 0, detent: 1)
        case "Chamfer": EditBarParam(label: "Distance", key: "distance", axis: nil, unit: "mm", min: 0, detent: 1)
        case "Shell": EditBarParam(label: "Thickness", key: "thickness", axis: nil, unit: "mm", min: 0.1, detent: 1)
        case "Cylinder", "Sphere", "Prism": EditBarParam(label: "Radius", key: "radius", axis: nil, unit: "mm", min: 0.1, detent: 1)
        case "Torus": EditBarParam(label: "Radius", key: "major_radius", axis: nil, unit: "mm", min: 0.1, detent: 1)
        case "Wedge": EditBarParam(label: "Height", key: "size", axis: "z", unit: "mm", min: 0.1, detent: 1)
        case "SweepRotate": EditBarParam(label: "To", key: "angle_hi_deg", axis: nil, unit: "°", min: -3600, detent: 5)
        case "Cone": EditBarParam(label: "Height", key: "height", axis: nil, unit: "mm", min: 0.1, detent: 1)
        case "SpurGear": EditBarParam(label: "Face width", key: "face_width", axis: nil, unit: "mm", min: 0.1, detent: 1)
        case "Cube": EditBarParam(label: "Height", key: "size", axis: "z", unit: "mm", min: 0.1, detent: 1)
        case "Revolve", "CircularPattern": EditBarParam(label: "Angle", key: "angle_deg", axis: nil, unit: "°", min: 0, detent: 5)
        default: nil
        }
    }

    func read(_ op: [String: Any]) -> Double {
        if let axis { return ((op[key] as? [String: Any])?[axis] as? NSNumber)?.doubleValue ?? 0 }
        return (op[key] as? NSNumber)?.doubleValue ?? 0
    }
}

/// Keynote-style edit bar beside the selection:
/// [Radius 4.2 mm ▲▼ | Stress −31% · +1.8 g | Done] with a drag handle.
struct SelectionEditBar: View {
    @Bindable var model: EditorModel
    let node: FeatureNode
    let param: EditBarParam
    let dismiss: () -> Void

    @State private var dragBase: Double?
    @State private var lastDetent: Int?
    @State private var text = ""
    @FocusState private var focused: Bool
    @State private var feedback = LiveFeedback()
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var value: Double { param.read(model.opDict(nodeId: node.nodeId) ?? [:]) }

    var body: some View {
        HStack(spacing: 10) {
            handle
            Text(param.label).font(.callout).foregroundStyle(.secondary)
            TextField(param.label, text: $text)
                .textFieldStyle(.roundedBorder)
                .font(.callout.monospacedDigit())
                .multilineTextAlignment(.trailing)
                .frame(width: 58)
                .focused($focused)
                .onSubmit { if let v = ScrubField.parse(text) { set(v, snapshot: true) } }
            Text(param.unit).font(.callout).foregroundStyle(.secondary)
            Stepper(param.label, value: Binding(get: { value }, set: { set($0, snapshot: true) }),
                    in: param.min...1_000_000, step: param.detent)
                .labelsHidden()
            if feedback.hasSomething {
                Divider().frame(height: 18)
                feedbackText
            }
            Divider().frame(height: 18)
            Button("Done", action: dismiss)
                .background {
                    Button("", action: dismiss).keyboardShortcut(.return, modifiers: [])
                        .frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
                        .disabled(focused)
                }
        }
        .controlSize(.small)
        .padding(.horizontal, 12).padding(.vertical, 7)
        .glassCapsule()
        .fixedSize()
        .onAppear { text = format(value); feedback.begin(model: model, node: node) }
        .onChange(of: value) { _, v in if !focused { text = format(v) } }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(node.name) edit bar")
    }

    /// Drag on the edge: scrub, with a trackpad alignment tick at each detent.
    private var handle: some View {
        Image(systemName: "line.3.horizontal")
            .rotationEffect(.degrees(90))
            .font(.caption.weight(.semibold)).foregroundStyle(.secondary)
            .frame(width: 14, height: 22).contentShape(Rectangle())
            .onHover { ($0 ? NSCursor.resizeLeftRight : NSCursor.arrow).set() }
            .gesture(DragGesture(minimumDistance: 1)
                .onChanged { g in
                    let first = dragBase == nil
                    let base = dragBase ?? value
                    if first { dragBase = base; lastDetent = Int((base / param.detent).rounded(.down)) }
                    let scale = NSEvent.modifierFlags.contains(.option) ? 0.01 : 0.1
                    let v = max(param.min, base + Double(g.translation.width) * scale * param.detent)
                    let d = Int((v / param.detent).rounded(.down))
                    if d != lastDetent {
                        lastDetent = d
                        NSHapticFeedbackManager.defaultPerformer.perform(.alignment, performanceTime: .now)
                    }
                    set(v, snapshot: first)
                }
                .onEnded { _ in dragBase = nil; lastDetent = nil; feedback.settle(model: model) })
            .help("Drag to adjust \(param.label.lowercased()) (⌥ fine)")
            .accessibilityLabel("Adjust \(param.label)")
            .accessibilityAdjustableAction { dir in
                set(max(param.min, value + (dir == .increment ? param.detent : -param.detent)), snapshot: true)
            }
    }

    @ViewBuilder private var feedbackText: some View {
        HStack(spacing: 4) {
            if let s = feedback.stressChange {
                Text("Stress \(signedPercent(s))")
            }
            if feedback.stressChange != nil && feedback.massDelta != nil { Text("·").foregroundStyle(.secondary) }
            if let g = feedback.massDelta {
                Text(feedback.massIsVolume ? "\(signed(g, 0)) mm³" : "\(signed(g, 1)) g")
            }
            if feedback.stressChange == nil && feedback.massDelta == nil && feedback.busy {
                Text("Solving…").foregroundStyle(.secondary)
            }
            if feedback.busy { ProgressView().controlSize(.mini) }
        }
        .font(.callout.monospacedDigit())
        .contentTransition(reduceMotion ? .identity : .numericText())
        .accessibilityElement(children: .combine)
    }

    private func set(_ v: Double, snapshot: Bool) {
        let clamped = max(param.min, v)
        if let axis = param.axis {
            model.editVec(nodeId: node.nodeId, key: param.key, axis: axis, value: clamped, snapshot: snapshot,
                          name: "Change \(param.label)")
        } else {
            model.editScalar(nodeId: node.nodeId, key: param.key, value: clamped, snapshot: snapshot,
                             name: "Change \(param.label)")
        }
        ReleaseWindowController.shared.refreshGeometry(model: model, collisions: snapshot)
        if !focused { text = format(clamped) }
        feedback.changed(model: model)
    }

    private func format(_ v: Double) -> String {
        abs(v - v.rounded()) < 0.001 ? String(Int(v.rounded())) : String(format: "%.1f", v)
    }
    private func signed(_ v: Double, _ digits: Int) -> String {
        let s = String(format: "%.\(digits)f", abs(v))
        return (v < 0 ? "−" : "+") + s
    }
    private func signedPercent(_ r: Double) -> String { signed(r * 100, 0) + "%" }
}

/// Live solver feedback for the edit bar: mass (or volume) delta from the
/// engine's inspect, and the stress change from a re-solve of the FEA study
/// when one is set up for this part. Both throttled; stale solves dropped.
@MainActor @Observable
final class LiveFeedback {
    var massDelta: Double?
    var massIsVolume = false
    var stressChange: Double?
    var busy = false
    /// Only show the feedback section once there is a number (or a solve).
    var hasSomething: Bool { busy || stressChange != nil || (massDelta.map { abs($0) > 1e-6 } ?? false) }

    @ObservationIgnored private var baseMass: (value: Double, isVolume: Bool)?
    @ObservationIgnored private var baseStress: Double?
    @ObservationIgnored private var partId: String?
    @ObservationIgnored private var pending: Task<Void, Never>?
    @ObservationIgnored private var feaTask: Task<Void, Never>?
    @ObservationIgnored private var generation = 0

    func begin(model: EditorModel, node: FeatureNode) {
        partId = (node.partIndex ?? model.selectedPartIndex).flatMap { model.partId(forPart: $0) }
        baseMass = measure(model)
        let a = AnalysisStore.shared
        if let r = a.feaResult, r.part_index == (node.partIndex ?? model.selectedPartIndex) { baseStress = r.max_von_mises_mpa }
    }

    func changed(model: EditorModel) {
        pending?.cancel()
        pending = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .milliseconds(120))
            guard let self, !Task.isCancelled else { return }
            if let base = self.baseMass, let now = self.measure(model), now.isVolume == base.isVolume {
                self.massDelta = now.value - base.value
                self.massIsVolume = now.isVolume
            }
            self.solveStress(model)
        }
    }

    /// Gesture ended: one more solve at the final value.
    func settle(model: EditorModel) { changed(model: model) }

    private func solveStress(_ model: EditorModel) {
        guard let base = baseStress, let data = AnalysisStore.documentData(model) else { return }
        generation += 1
        let gen = generation
        let req = AnalysisStore.shared.request()
        busy = true
        feaTask?.cancel()
        feaTask = Task { @MainActor [weak self] in
            let r = await Task.detached { try? AnalysisFFI.feaSolve(doc: data, request: req) }.value
            guard let self, gen == self.generation else { return }
            self.busy = false
            if let r, base > 0 { self.stressChange = r.max_von_mises_mpa / base - 1 }
        }
    }

    /// Mass in grams from the engine's inspect of the part, else volume (mm³).
    private func measure(_ model: EditorModel) -> (value: Double, isVolume: Bool)? {
        guard let id = partId, let engine = model.agentEnsureEngine(),
              let text = try? engine.agentRead("inspect", ["part_id": id]) else { return nil }
        if let g = Self.number(after: "mass", in: text) { return (g, false) }
        if let v = Self.number(after: "volume", in: text) { return (v, true) }
        return nil
    }

    static func number(after key: String, in text: String) -> Double? {
        let lower = text.lowercased()
        guard let r = lower.range(of: key) else { return nil }
        let tail = lower[r.upperBound...]
        guard let start = tail.firstIndex(where: { $0.isNumber || $0 == "-" }) else { return nil }
        let digits = tail[start...].prefix { $0.isNumber || $0 == "." || $0 == "-" || $0 == "e" }
        return Double(digits)
    }
}

/// Positions the edit bar beside the selected part, following the camera.
struct EditBarOverlay: View {
    @Bindable var model: EditorModel
    @State private var dismissedNode: String?

    var body: some View {
        // Read the camera so the bar follows orbit/zoom/pan.
        _ = (model.azimuth, model.elevation, model.distance, model.panOffset)
        return GeometryReader { geo in
            if let node = editableNode, let param = EditBarParam.of(node.opType), dismissedNode != node.id,
               !model.sketching, !GlassUI.shared.historyOpen, model.activeFeatureOp == nil {
                let p = anchor(node, in: geo.size)
                SelectionEditBar(model: model, node: node, param: param) { dismissedNode = node.id }
                    .id(node.id)
                    .modifier(ChromeRegion(key: "editBar"))
                    .position(p)
                    .transition(.opacity.combined(with: .scale(scale: 0.96)))
            }
        }
        .onChange(of: model.selectedFeatureID) { _, _ in dismissedNode = nil }
    }

    private var editableNode: FeatureNode? {
        guard model.usesDocumentTree, let n = model.selectedFeatureNode,
              EditBarParam.of(n.opType) != nil else { return nil }
        return n
    }

    /// Beside the part's projected bounds — never on top of the part — and
    /// clear of the floating panels and the dock.
    private func anchor(_ node: FeatureNode, in size: CGSize) -> CGPoint {
        let barHalf: CGFloat = 190, gap: CGFloat = 20
        let left = Glass.panelWidth + 2 * Glass.inset + barHalf
        let right = size.width - Glass.panelWidth - 2 * Glass.inset - barHalf
        let fallback = CGPoint(x: size.width / 2, y: Glass.toolbarHeight + 40)
        guard let pi = node.partIndex ?? model.selectedPartIndex,
              let r = AgentShimmerOverlay.rect(pi, pad: 0) else { return fallback }
        let y = min(max(r.midY, Glass.toolbarHeight + 30), size.height - 120)
        if r.maxX + gap + barHalf <= right + barHalf { return CGPoint(x: max(left, r.maxX + gap + barHalf), y: y) }
        if r.minX - gap - barHalf >= left { return CGPoint(x: r.minX - gap - barHalf, y: y) }
        // No room either side: above the part.
        return CGPoint(x: min(max(r.midX, left), max(left, right)), y: max(Glass.toolbarHeight + 30, r.minY - 30))
    }
}

// MARK: - History filmstrip (signature moment 2)

/// The dock's clock: a filmstrip of the document's named steps with a
/// playhead. Scrubbing rewinds/replays through the undo history and the
/// viewport rebuilds live. When a rebuild is slow (the last one took longer
/// than the live budget), scrubbing only previews the frame and the rebuild
/// happens on release, so the gesture never stalls behind the kernel.
struct HistoryFilmstrip: View {
    @Bindable var model: EditorModel
    @Bindable var ui = GlassUI.shared
    @State private var hover: Int?
    @State private var preview: Int?
    @State private var rebuildMillis: Double?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// Live budget: rebuilds slower than this are deferred to release.
    static let liveBudgetMillis: Double = 250

    /// Frame 0 is the document as opened; frame k is after the k-th step.
    private var frames: [String] {
        ["Opened"] + model.undoStack.map(\.name) + model.redoStack.reversed().map(\.name)
    }
    private var playhead: Int { model.undoStack.count }
    private var live: Bool { (rebuildMillis ?? ui.lastSeekMillis ?? ui.lastBuildMillis) <= Self.liveBudgetMillis }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("History").font(.headline)
                Spacer()
                Text(status).font(.caption).foregroundStyle(.secondary).monospacedDigit()
                if playhead < frames.count - 1 {
                    Button("Return to Latest") { scrub(to: frames.count - 1) }.controlSize(.small)
                }
                Button { withAnimation(glassSpring(reduceMotion)) { ui.historyOpen = false } } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(.tertiary)
                }.buttonStyle(.borderless).help("Close History").accessibilityLabel("Close History")
            }
            GeometryReader { geo in
                let w: CGFloat = 96
                ScrollViewReader { proxy in
                    ScrollView(.horizontal) {
                        VStack(alignment: .leading, spacing: 4) {
                            // The playhead rides its own track above the frames.
                            ZStack(alignment: .leading) {
                                Capsule().fill(.quaternary).frame(height: 4)
                                Capsule().fill(Color.primary.opacity(0.5))
                                    .frame(width: CGFloat(preview ?? playhead) * (w + 4) + w / 2, height: 4)
                                Circle().fill(Color.primary).frame(width: 12, height: 12)
                                    .offset(x: CGFloat(preview ?? playhead) * (w + 4) + w / 2 - 6)
                                    .accessibilityHidden(true)
                            }
                            .frame(width: CGFloat(frames.count) * (w + 4), height: 12)
                            HStack(spacing: 4) {
                                ForEach(Array(frames.enumerated()), id: \.offset) { i, name in
                                    frame(i, name, width: w).id(i)
                                }
                            }
                        }
                        .contentShape(Rectangle())
                        .gesture(DragGesture(minimumDistance: 0).onChanged { g in
                            let i = index(at: g.location.x, width: w)
                            preview = i
                            if live && i != playhead { scrub(to: i) }
                        }.onEnded { g in
                            let i = index(at: g.location.x, width: w)
                            preview = nil
                            if i != playhead { scrub(to: i) }
                        })
                    }
                    .scrollIndicators(.hidden)
                    .onAppear { proxy.scrollTo(playhead, anchor: .center) }
                    .frame(width: geo.size.width)
                }
            }.frame(height: 98)
        }
        .padding(12)
        .frame(width: 600)
        .glassPanel()
        .accessibilityElement(children: .contain)
        .accessibilityLabel("History")
        .accessibilityAdjustableAction { dir in
            scrub(to: max(0, min(frames.count - 1, playhead + (dir == .increment ? 1 : -1))))
        }
    }

    private var status: String {
        let step = "Step \(preview ?? playhead) of \(frames.count - 1)"
        guard let ms = rebuildMillis else { return step }
        return live ? "\(step) · rebuilt in \(Int(ms)) ms"
                    : "\(step) · \(String(format: "%.1f", ms / 1000)) s rebuild — applies on release"
    }

    private func frame(_ i: Int, _ raw: String, width: CGFloat) -> some View {
        let current = i == playhead
        let future = i > playhead
        // "Claude: Create tab" → sparkle badge + "Create tab".
        let byAgent = raw.hasPrefix("Claude: ")
        let name = byAgent ? String(raw.dropFirst(8)) : raw
        return VStack(spacing: 3) {
            Group {
                if let img = ui.thumbnails[i] {
                    Image(nsImage: img).resizable().aspectRatio(contentMode: .fill)
                } else {
                    Image(systemName: i == 0 ? "doc" : "square.stack.3d.up").font(.system(size: 15))
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(.quaternary.opacity(0.5))
                }
            }
            .frame(width: width, height: 52)
            .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
            .overlay(alignment: .topTrailing) {
                if byAgent {
                    Image(systemName: "sparkle").font(.caption2)
                        .padding(3).background(.ultraThinMaterial, in: Circle()).padding(3)
                        .help("Made by the agent")
                }
            }
            .overlay(RoundedRectangle(cornerRadius: 6, style: .continuous)
                .strokeBorder(Color.primary.opacity(current ? 0.8 : (hover == i ? 0.3 : 0)), lineWidth: current ? 2 : 1))
            Text(name).font(.caption2).lineLimit(1).truncationMode(.tail).frame(width: width)
        }
        .opacity(future ? 0.45 : 1)
        .onHover { hover = $0 ? i : (hover == i ? nil : hover) }
        .help(i == 0 ? "The document as opened" : raw)
        .accessibilityLabel("\(raw)\(current ? ", current" : "")\(future ? ", undone" : "")")
    }

    private func index(at x: CGFloat, width: CGFloat) -> Int {
        max(0, min(frames.count - 1, Int(x / (width + 4))))
    }

    /// Move through the undo history to frame `i`, then time the rebuild.
    private func scrub(to i: Int) {
        Task { @MainActor in rebuildMillis = await GlassUI.seek(model: model, to: i) }
    }
}

extension GlassUI {
    /// Rewind/replay the undo history to `frame` (0 = as opened) and rebuild
    /// the viewport. Returns the wall time until the rebuilt scene landed, ms
    /// (including a background kernel solve when the document needs one).
    /// Pulse a soft violet glow on the suggestion's parts until it is
    /// accepted or dismissed. Reduce Motion holds a steady glow instead.
    func startShimmer(model: EditorModel) {
        shimmerTask?.cancel()
        shimmerTask = Task { @MainActor in
            let rw = ReleaseWindowController.shared
            let still = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
            let t0 = Date()
            while !Task.isCancelled, GlassUI.shared.suggestion != nil, !model.agentShimmer.isEmpty {
                let t = Date().timeIntervalSince(t0)
                let level = still ? 0.12 : Float(0.05 + 0.13 * (sin(t * 2.4) + 1) / 2)
                rw.glow(parts: model.agentShimmer, level: level)
                try? await Task.sleep(for: .milliseconds(still ? 500 : 33))
            }
            rw.repaintAll()
        }
    }

    /// Snapshot the viewport as history frame `frame`'s thumbnail once the
    /// rebuild has landed. Called after every history change.
    func captureThumbnail(model: EditorModel) {
        let frame = model.undoStack.count
        let total = frame + model.redoStack.count
        thumbnails = thumbnails.filter { $0.key <= total }
        Task { @MainActor in
            var waited = 0
            while (model.geometryDirty || model.solving) && waited < 600 {
                try? await Task.sleep(for: .milliseconds(50)); waited += 1
            }
            try? await Task.sleep(for: .milliseconds(80))
            guard model.undoStack.count == frame, let ar = ReleaseWindowController.shared.arView else { return }
            RenderGate.wake(for: 2)  // a paused view never delivers a snapshot
            ar.snapshot(saveToHDR: false) { img in
                guard let img else { return }
                let thumb = NSImage(size: NSSize(width: 96, height: 60))
                thumb.lockFocus()
                let src = img.size, aspect = 96.0 / 60.0
                let w = min(src.width, src.height * aspect), h = w / aspect
                img.draw(in: NSRect(x: 0, y: 0, width: 96, height: 60),
                         from: NSRect(x: (src.width - w) / 2, y: (src.height - h) / 2, width: w, height: h),
                         operation: .copy, fraction: 1)
                thumb.unlockFocus()
                Task { @MainActor in GlassUI.shared.thumbnails[frame] = thumb }
            }
        }
    }

    static func seek(model: EditorModel, to frame: Int) async -> Double {
        let started = Date()
        let from = model.undoStack.count
        while model.undoStack.count > frame, model.canUndo { model.undo() }
        while model.undoStack.count < frame, model.canRedo { model.redo() }
        // Seeking to the frame we're on changes nothing: return at once.
        // Waiting on `geometryDirty` here stalled every no-op seek for the
        // full 60 s cap whenever the viewport had a pending (unconsumed)
        // rebuild — e.g. a window that isn't on screen (fuzz F2).
        guard model.undoStack.count != from || model.solving else {
            let ms = Date().timeIntervalSince(started) * 1000
            GlassUI.shared.lastSeekMillis = ms
            return ms
        }
        ReleaseWindowController.shared.refreshGeometry(model: model, collisions: false)
        // Wait for the viewport to adopt the scene (geometryDirty clears) and
        // for any background solve to finish — capped so a pathological
        // document cannot wedge the gesture.
        let deadline = Date().addingTimeInterval(60)
        repeat {
            try? await Task.sleep(for: .milliseconds(16))
        } while (model.geometryDirty || model.solving) && Date() < deadline
        let ms = Date().timeIntervalSince(started) * 1000
        GlassUI.shared.lastSeekMillis = ms
        return ms
    }
}

// MARK: - Agent suggestion pill (signature moment 3)

/// One proposed edit from the agent on the local MCP bridge: a sentence, the
/// edit tool call it would make, and the parts it touches (for the shimmer).
struct AgentSuggestion: Identifiable, Equatable {
    let id = UUID()
    let text: String
    let tool: String
    let args: [String: AnyHashable]
    let parts: [Int]

    static func == (a: AgentSuggestion, b: AgentSuggestion) -> Bool { a.id == b.id }
}

/// A single pill with an Apple Intelligence gradient edge:
/// "Add 4 M3 counterbores on the motor bolt circle?" [Add] [×]
struct AgentSuggestionPill: View {
    @Bindable var model: EditorModel
    let suggestion: AgentSuggestion
    @State private var error: String?
    @State private var phase: Double = 0
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "sparkle").foregroundStyle(gradient)
                .accessibilityHidden(true)
            Text(error ?? suggestion.text).font(.callout).lineLimit(2)
                .foregroundStyle(error == nil ? .primary : .secondary)
            Button("Add") { accept() }.buttonStyle(.bordered).controlSize(.small)
                .help("Apply the suggested edit (one undo step)")
            Button { dismiss() } label: { Image(systemName: "xmark") }
                .buttonStyle(.borderless).controlSize(.small)
                .help("Dismiss suggestion").accessibilityLabel("Dismiss suggestion")
        }
        .padding(.leading, 14).padding(.trailing, 8).padding(.vertical, 7)
        .glassCapsule()
        .overlay {
            Capsule(style: .continuous)
                .strokeBorder(AngularGradient(colors: Self.colors, center: .center,
                                              angle: .degrees(reduceMotion ? 0 : phase)),
                              lineWidth: 1.8)
                .allowsHitTesting(false)
        }
        .frame(maxWidth: 560)
        .fixedSize(horizontal: false, vertical: true)
        .onAppear {
            guard !reduceMotion else { return }
            withAnimation(.linear(duration: 6).repeatForever(autoreverses: false)) { phase = 360 }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Agent suggestion: \(suggestion.text)")
    }

    static let colors: [Color] = [
        Color(red: 0.98, green: 0.55, blue: 0.24), Color(red: 0.95, green: 0.32, blue: 0.55),
        Color(red: 0.62, green: 0.36, blue: 0.96), Color(red: 0.25, green: 0.55, blue: 0.98),
        Color(red: 0.98, green: 0.55, blue: 0.24),
    ]
    private var gradient: LinearGradient {
        LinearGradient(colors: Self.colors, startPoint: .leading, endPoint: .trailing)
    }

    private func accept() {
        // The suggested edit runs through the MCP core, exactly as the agent's
        // own call would have.
        let tool = suggestion.tool, args = suggestion.args.mapValues { $0 as Any }
        Task { @MainActor in
            let r = await AgentCore.shared.call(tool, args)
            if r["isError"] as? Bool == true {
                error = ((r["content"] as? [[String: Any]])?.first?["text"] as? String) ?? "edit failed"
                return
            }
            ReleaseWindowController.shared.refreshGeometry(model: model)
            dismiss()
        }
    }

    private func dismiss() {
        GlassUI.shared.suggestion = nil
        model.agentShimmer = []
        ReleaseWindowController.shared.repaintAll()
    }
}

/// Screen-space helpers for the parts a pending suggestion touches. The
/// shimmer itself is a material glow on the part (see `GlassUI.startShimmer`),
/// never an outline box, cursor, or card.
@MainActor
enum AgentShimmerOverlay {
    /// Screen rect of a part's bounds, in overlay coordinates.
    static func rect(_ pi: Int, pad: CGFloat = 18) -> CGRect? {
        let rw = ReleaseWindowController.shared
        guard let ar = rw.arView,
              let e = rw.centeringEntity?.findEntity(named: "part\(pi)") ?? rw.centeringEntity?.findEntity(named: "inst\(pi)")
        else { return nil }
        let b = e.visualBounds(relativeTo: nil)
        var pts: [CGPoint] = []
        for dx in [-1, 1] as [Float] { for dy in [-1, 1] as [Float] { for dz in [-1, 1] as [Float] {
            let c = b.center + SIMD3(dx, dy, dz) * b.extents / 2
            if let p = ar.project(c) { pts.append(CGPoint(x: p.x, y: ar.isFlipped ? p.y : ar.bounds.height - p.y)) }
        }}}
        guard let minX = pts.map(\.x).min(), let maxX = pts.map(\.x).max(),
              let minY = pts.map(\.y).min(), let maxY = pts.map(\.y).max() else { return nil }
        return CGRect(x: minX, y: minY, width: maxX - minX, height: maxY - minY).insetBy(dx: -pad, dy: -pad)
    }
}

// MARK: - Context menu

/// Right-click: the same tools as the dock, as a standard menu.
struct DockContextMenu: View {
    @Bindable var model: EditorModel
    var body: some View {
        ForEach(DockTools.tools(GlassUI.shared.space, model: model)) { t in
            Button { t.action() } label: { Label(t.label, systemImage: t.symbol) }.disabled(!t.enabled)
        }
    }
}


// MARK: - Non-modal command panel, toast, cheat sheet

/// A feature command (fillet, shell, pattern…) as a floating glass panel
/// beneath the dock — not a sheet, so the viewport stays live: orbit, pick
/// edges, change the selection while it is open. Return applies, Esc cancels.
struct CommandPanelHost: View {
    @Bindable var model: EditorModel
    var body: some View {
        if let op = model.activeFeatureOp {
            FeatureOpSheet(model: model, op: op)
                .fixedSize(horizontal: false, vertical: true)
                .glassPanel()
                .id(op)
                .transition(.move(edge: .top).combined(with: .opacity))
                .modifier(ChromeRegion(key: "commandPanel"))
        }
    }
}

/// The command palette as a Spotlight-style floating field, not a sheet.
struct PaletteHost: View {
    @Bindable var model: EditorModel
    var body: some View {
        if model.showsCommandPalette {
            CommandPaletteSheet(model: model)
                .glassPanel()
                .transition(.scale(scale: 0.97).combined(with: .opacity))
                .modifier(ChromeRegion(key: "palette"))
        }
    }
}

/// A part the kernel refused to build (a whole-part fillet on a body with
/// inside corners) is missing from the viewport; say why, and offer the way
/// back, for as long as it stays missing. Without this the viewport just
/// went blank.
struct PartFailureHost: View {
    @Bindable var model: EditorModel
    var body: some View {
        if let message = model.partFailureMessage {
            HStack(spacing: Theme.Space.s) {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                Text(message).font(.callout).lineLimit(3).fixedSize(horizontal: false, vertical: true)
                Button("Undo") { model.undo() }.buttonStyle(.borderless)
            }
            .padding(.horizontal, 12).padding(.vertical, 6)
            .frame(maxWidth: 560)
            .glassCapsule()
            .transition(.opacity.combined(with: .move(edge: .top)))
            .modifier(ChromeRegion(key: "partFailure"))
        }
    }
}

/// "Undo Fillet" and friends: a quiet pill that fades on its own.
struct ToastHost: View {
    @Bindable var ui = GlassUI.shared
    var body: some View {
        if let t = ui.toast {
            Label(t.text, systemImage: t.symbol)
                .font(.callout)
                .padding(.horizontal, 12).padding(.vertical, 6)
                .glassCapsule()
                .transition(.opacity.combined(with: .move(edge: .top)))
                .accessibilityAddTraits(.updatesFrequently)
                .allowsHitTesting(false)
                .id(t.text)
        }
    }
}

/// Every shortcut for the current workspace, on one glass card (? toggles).
struct ShortcutSheet: View {
    @Bindable var model: EditorModel
    @Bindable var ui = GlassUI.shared

    private static let global: [(String, String)] = [
        ("⌘Z / ⇧⌘Z", "Undo / Redo"), ("⌘F  ⌘K", "Search"), ("⇧⌘P", "Command palette"),
        ("⌃⌘1–5", "Switch workspace"), ("⌥⌘1 / ⌥⌘2", "Navigator / Inspector"),
        ("⌘1–4", "Iso / Front / Right / Top view"), ("⌘0", "Frame all"),
        ("⌃M", "Measure"), ("⌥⌘X", "Section view"), ("⌫", "Delete selection"),
        ("Esc", "Cancel / deselect"), ("?", "This sheet"),
    ]

    /// Sketch-mode tool keys, in bar order (live only while sketching).
    static let sketch: [(key: String, tool: SketchToolKind)] =
        (SketchBar.primaryTools + SketchBar.overflowTools + [.dimension])
            .compactMap { t in t.shortcut.map { (String($0).uppercased(), t) } }

    var body: some View {
        let tools = DockTools.tools(ui.space, model: model).filter { $0.shortcut != nil }
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            HStack {
                Text("Keyboard Shortcuts").font(.headline)
                Spacer()
                Button { BugReporter.reportBug() } label: { Label("Report a Bug…", systemImage: "ladybug") }
                    .buttonStyle(.borderless).foregroundStyle(.secondary)
                    .help("Opens a pre-filled GitHub issue — nothing is sent automatically")
                Button { ui.shortcutsShown = false } label: { Image(systemName: "xmark.circle.fill").foregroundStyle(.tertiary) }
                    .buttonStyle(.borderless).help("Close").accessibilityLabel("Close shortcuts")
            }
            HStack(alignment: .top, spacing: 28) {
                VStack(alignment: .leading, spacing: 6) {
                    Eyebrow(ui.space.label)
                    ForEach(tools) { t in row(String(t.shortcut!).uppercased(), t.label, symbol: t.symbol) }
                }
                VStack(alignment: .leading, spacing: 6) {
                    Eyebrow("Sketch")
                    ForEach(Self.sketch, id: \.key) { row($0.key, $0.tool.label, symbol: $0.tool.symbol) }
                }
                VStack(alignment: .leading, spacing: 6) {
                    Eyebrow("Everywhere")
                    ForEach(Self.global, id: \.0) { row($0.0, $0.1) }
                }
                VStack(alignment: .leading, spacing: 6) {
                    // The camera bindings follow Settings ▸ Navigation.
                    Eyebrow("Navigation · \(NavigationScheme.current.label)")
                    ForEach(NavigationScheme.current.summary, id: \.gesture) { row($0.gesture, $0.action) }
                }
            }
        }
        .padding(18)
        .frame(width: 900)
        .glassPanel()
        .modifier(ChromeRegion(key: "shortcuts"))
    }

    private func row(_ key: String, _ label: String, symbol: String? = nil) -> some View {
        HStack(spacing: 8) {
            Text(key).font(.callout.monospaced()).padding(.horizontal, 6).padding(.vertical, 1)
                .background(.quaternary, in: RoundedRectangle(cornerRadius: 5, style: .continuous))
                .frame(minWidth: 64, alignment: .leading)
            if let symbol { Image(systemName: symbol).frame(width: 16).foregroundStyle(.secondary) }
            Text(label).font(.callout)
        }
    }
}


/// A request for the floating panel.
struct PanelRequest: Identifiable {
    let id = UUID()
    let title: String
    let symbol: String
    let width: CGFloat
    let onClose: () -> Void
    let content: () -> AnyView
}

/// Hosts `GlassUI.panel` beneath the dock: title, close (Esc), content.
struct FloatingPanelHost: View {
    @Bindable var ui = GlassUI.shared
    var body: some View {
        if let p = ui.panel {
            VStack(alignment: .leading, spacing: Theme.Space.m) {
                HStack(spacing: Theme.Space.s) {
                    Image(systemName: p.symbol).foregroundStyle(.secondary)
                    Text(p.title).font(.headline)
                    Spacer()
                    Button { ui.closePanel() } label: { Image(systemName: "xmark.circle.fill").foregroundStyle(.tertiary) }
                        .buttonStyle(.borderless)
                        .keyboardShortcut(.cancelAction)
                        .help("Close (Esc)").accessibilityLabel("Close \(p.title)")
                }
                ScrollView { p.content().frame(maxWidth: .infinity, alignment: .leading) }
                    .frame(maxHeight: 560)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(16)
            .frame(width: p.width)
            .glassPanel()
            .id(p.id)
            .transition(.move(edge: .top).combined(with: .opacity))
            .modifier(ChromeRegion(key: "floatingPanel"))
        }
    }
}
