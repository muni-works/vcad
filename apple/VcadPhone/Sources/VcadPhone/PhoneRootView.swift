import SwiftUI
import QuickLook

/// The Mac's released window, squished onto a phone:
///
/// - the viewport fills the screen
/// - top: document, undo/redo, search, and a ⋯ menu that is the Mac's menu
///   bar (File, Tools, Select, Camera, panels); beneath it the Mac's own
///   command panel (fillet/shell/…), floating panel, command palette, toasts
/// - bottom: the Mac's dock (GlassDock — workspaces, tools, history, all
///   tools), which turns into the sketch bar while sketching
/// - above the dock: the Mac's floating panels (navigator, measure, section,
///   assembly, pose/simulation, analysis/fab, the Object Inspector), stacked
///   as cards at phone width. Selecting something opens the inspector.
struct PhoneRootView: View {
    @Bindable var model: EditorModel
    @Bindable var intent: IntentEngine
    @Bindable private var flow = PhoneDocumentFlow.shared
    @Bindable private var ui = GlassUI.shared
    @State private var contextPart: Int?
    @State private var exportDoc: VcadExportDocument?
    @State private var composerShown = false

    var body: some View {
        ZStack(alignment: .top) {
            PhoneViewport(model: model, onLongPress: { contextPart = $0 })
            VStack(spacing: 8) {
                topBar
                ToastHost()
                PartFailureHost(model: model)
                CommandPanelHost(model: model)
                FloatingPanelHost()
                PaletteHost(model: model)
                if model.solving || model.loading?.isEvaluating == true {
                    ProgressView().controlSize(.small).padding(6).glassCapsule()
                }
                Spacer(minLength: 0)
                // Selection readout: "2 faces · 1 240 mm²" / "1 edge · 42.0 mm".
                if !model.sketching, let readout = model.topologySelectionReadout() {
                    Text(readout)
                        .font(.system(size: 12, weight: .medium).monospacedDigit())
                        .foregroundStyle(.secondary)
                        .padding(.horizontal, 10).padding(.vertical, 5)
                        .background(.regularMaterial, in: Capsule())
                        .allowsHitTesting(false)
                        .accessibilityIdentifier("selectionReadout")
                }
                panelStack
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 8)
            .animation(Motion.smooth, value: model.activeFeatureOp)
            .animation(Motion.smooth, value: model.showsCommandPalette)
            .animation(Motion.smooth, value: ui.panel?.id)
            .animation(Motion.smooth, value: ui.toast?.text)
            .animation(Motion.panel, value: panelKey)
        }
        .safeAreaInset(edge: .bottom, spacing: 0) { bottomChrome }
        .confirmationDialog(contextTitle, isPresented: contextShown, titleVisibility: .visible) {
            if let pi = contextPart { contextActions(pi) }
        }
        .fileImporter(isPresented: $flow.importing, allowedContentTypes: PhoneDocuments.openableTypes) { result in
            if case let .success(url) = result { PhoneDocuments.open(url, into: model) }
        }
        .fileExporter(isPresented: exportShown, document: exportDoc,
                      contentType: exportDoc?.contentType ?? PhoneDocuments.vcadType,
                      defaultFilename: exportDoc?.filename ?? model.documentName) { result in
            if case .success = result { GlassUI.shared.flash("Exported", symbol: "square.and.arrow.up") }
            exportDoc = nil
        }
        .quickLookPreview($flow.quickLookURL)
        .alert("Couldn’t save", isPresented: Binding(get: { model.saveError != nil },
                                                     set: { if !$0 { model.saveError = nil } })) {
            Button("OK") { model.saveError = nil }
        } message: { Text(model.saveError ?? "") }
        // Selecting something opens its inspector (the Mac's right rail is
        // always there; a phone has room for it only on demand).
        .onChange(of: selectionKey) { _, _ in
            if !model.hasSelection { flow.inspectorShown = false; return }
            if !model.sketching, model.armedShape == nil, model.pendingBoolean == nil { flow.inspectorShown = true }
        }
        // A card that just opened comes to the front.
        .onChange(of: panelKey, initial: true) { _, _ in
            let now = openCards
            if let added = now.first(where: { !lastOpen.contains($0) }) { frontCard = added }
            lastOpen = now
        }
        // The Mac overlay's viewport hooks (ReleasedOverlayView), for touch.
        .onChange(of: model.section) { _, _ in applySectionView(model: model) }
        .onChange(of: model.picking.revision) { _, _ in PhonePick.refreshTopologyOverlay(model: model) }
        .onReceive(NotificationCenter.default.publisher(for: .analysisMeshesReady)) { _ in
            PhonePick.refreshTopologyOverlay(model: model)
        }
        .onChange(of: model.documentRevision, initial: true) { _, _ in model.prewarmPicking() }
        .onChange(of: model.measuring) { _, on in
            if !on { model.picking.filter = .standard }
            PhonePick.refreshTopologyOverlay(model: model)
        }
        .onChange(of: model.sketching) { _, _ in PhonePick.refreshSketchInk(model: model, force: true) }
        .onChange(of: model.undoStack.count, initial: true) { _, _ in GlassUI.shared.captureThumbnail(model: model) }
        .onChange(of: model.source) { _, _ in
            GlassUI.shared.thumbnails = [:]
            GlassUI.shared.captureThumbnail(model: model)
        }
    }

    private var selectionKey: String {
        "\(String(describing: model.selectedFeatureID))|\(String(describing: model.selectedInstanceIndex))|\(model.multiSelectedParts.sorted())"
    }

    // MARK: top bar

    private var topBar: some View {
        HStack(spacing: 6) {
            documentMenu
            Spacer(minLength: 4)
            barButton("arrow.uturn.backward", "Undo", enabled: model.canUndo) { model.undo() }
            barButton("arrow.uturn.forward", "Redo", enabled: model.canRedo) { model.redo() }
            barButton("magnifyingglass", "Find Command", enabled: true) { model.showsCommandPalette.toggle() }
            Menu { menuBar } label: {
                Image(systemName: "ellipsis").font(.system(size: 16, weight: .semibold))
                    .frame(width: 36, height: 36).glassCapsule()
            }
            .accessibilityIdentifier("fileMenu")
        }
    }

    private var documentMenu: some View {
        Menu {
            Button { flow.importing = true } label: { Label("Open from Files…", systemImage: "folder") }
            Button { model.newDocument() } label: { Label("New", systemImage: "doc.badge.plus") }
            let recents = PhoneDocuments.recents.prefix(8)
            if !recents.isEmpty {
                Section("On This iPhone") {
                    ForEach(Array(recents), id: \.self) { url in
                        Button(url.deletingPathExtension().lastPathComponent) { PhoneDocuments.open(url, into: model) }
                    }
                }
            }
            let samples = PhoneDocuments.samples
            if !samples.isEmpty {
                Section("Samples") {
                    ForEach(samples, id: \.self) { url in
                        Button(url.deletingPathExtension().lastPathComponent) { PhoneDocuments.open(url, into: model) }
                    }
                }
            }
        } label: {
            HStack(spacing: 5) {
                Image(systemName: "doc")
                Text(model.documentName.isEmpty ? "vcad" : model.documentName)
                    .lineLimit(1).truncationMode(.middle)
                if model.documentDirty { Circle().fill(.orange).frame(width: 6, height: 6) }
            }
            .font(.subheadline.weight(.medium))
            .padding(.horizontal, 12).frame(height: 36)
            .glassCapsule()
        }
        .accessibilityIdentifier("documentMenu")
    }

    /// The Mac's menu bar (Shell.swift `EditorCommands`), as one menu.
    @ViewBuilder private var menuBar: some View {
        // File
        Section {
            Button { save() } label: { Label("Save", systemImage: "square.and.arrow.down") }
                .disabled(!model.usesDocumentTree && !model.source.isSandbox)
            Button { saveAsPanel(model) } label: { Label("Save As…", systemImage: "doc.on.doc") }
                .disabled(!model.usesDocumentTree)
            Button { model.revertDocument() } label: { Label("Revert to Saved", systemImage: "arrow.counterclockwise") }
                .disabled(!model.usesDocumentTree || !model.documentDirty)
            Menu {
                Button("vcad (a copy)…") { exportCopy() }.disabled(model.documentJSON == nil)
                ForEach(DocumentExportFormat.allCases, id: \.self) { f in
                    Button("\(f.label)…") { export(f) }
                        .disabled(f == .stl ? !model.canExport : !model.canExportDocument)
                }
            } label: { Label("Export", systemImage: "square.and.arrow.up") }
            Button { exportUSDZPanel(model) } label: { Label("View in AR", systemImage: "arkit") }
                .disabled(!model.canExport)
        }
        // Tools
        Section {
            Toggle(isOn: $flow.simulationShown) { Label("Simulate", systemImage: "play.circle") }
                .disabled(!model.canSimulate && !model.hasPoseStudio)
            Toggle(isOn: $flow.materialsShown) { Label("Materials", systemImage: "paintpalette") }
            Button { model.showsParameterTable = true } label: { Label("Parameters…", systemImage: "slider.horizontal.3") }
                .disabled(!model.usesDocumentTree)
            Toggle(isOn: $model.measuring) { Label("Measure", systemImage: "ruler") }
                .disabled(!model.usesDocumentTree)
            Toggle(isOn: Binding(get: { model.section != nil }, set: { _ in model.toggleSmartSection() })) {
                Label("Section View", systemImage: "square.split.diagonal")
            }
            Toggle(isOn: Binding(get: { AnalysisStore.shared.shown },
                                 set: { AnalysisStore.shared.shown = $0; AnalysisStore.shared.overlayRevision += 1 })) {
                Label("Analysis", systemImage: "waveform.path.ecg")
            }
            .disabled(!model.usesDocumentTree)
            if IntentEngine.isAvailable {
                Button { composerShown = true; intent.focusRequested = true } label: {
                    Label("Describe a Part…", systemImage: "sparkles")
                }
            }
        }
        // View
        Section {
            Toggle(isOn: $model.showsTree) { Label("Model Navigator", systemImage: "sidebar.leading") }
            Toggle(isOn: $flow.inspectorShown) { Label("Inspector", systemImage: "sidebar.trailing") }
            Menu {
                ForEach(PickFilter.allCases) { f in
                    Toggle(f.label, isOn: Binding(get: { model.picking.filter == f },
                                                  set: { if $0 { model.picking.filter = f } }))
                }
                Divider()
                Button("Clear Face/Edge Selection") { model.picking.clear() }
                    .disabled(model.picking.selection.isEmpty)
            } label: { Label("Select", systemImage: "cursorarrow.rays") }
            Menu {
                Button("Isometric") { model.animateCamera(to: .isometric) }
                Button("Front") { model.animateCamera(to: .front) }
                Button("Right") { model.animateCamera(to: .right) }
                Button("Top") { model.animateCamera(to: .top) }
                Divider()
                Button("Frame All") { model.resetCamera(animated: true) }
                Button("Frame Selection") { ReleaseWindowController.shared.frameSelection() }
                    .disabled(!model.hasSelection)
                Divider()
                Toggle("Zebra Analysis", isOn: $model.zebraMode)
            } label: { Label("Camera", systemImage: "camera") }
            if model.hasHiddenParts {
                Button { model.showAllParts() } label: { Label("Show All Parts", systemImage: "eye") }
            }
        }
    }

    private func barButton(_ symbol: String, _ label: String, enabled: Bool, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol).font(.system(size: 15, weight: .semibold))
                .frame(width: 36, height: 36).glassCapsule()
        }
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.45)
        .accessibilityLabel(label)
    }

    // MARK: panels above the dock

    /// The Mac's floating panels. A phone has room for one at a time: the
    /// most recently opened is in front, the rest wait as chips above it.
    enum Card: String, CaseIterable, Identifiable {
        case inspector, analysis, fab, tune, sound, sketch, measure, section, materials, simulation, assembly, navigator
        var id: String { rawValue }
        var title: String {
            switch self {
            case .inspector: "Inspector"
            case .analysis: "Analysis"
            case .fab: "Fabricate"
            case .tune: "Tune"
            case .sound: "Rings At"
            case .sketch: "Constraints"
            case .measure: "Measure"
            case .section: "Section"
            case .materials: "Material"
            case .simulation: "Simulate"
            case .assembly: "Joints"
            case .navigator: "Model"
            }
        }
    }
    @State private var frontCard: Card?
    @State private var lastOpen: [Card] = []

    /// Every card whose Mac panel would be on screen right now.
    private var openCards: [Card] {
        let gripper = model.source.isGripper
        var out: [Card] = []
        if flow.inspectorShown { out.append(.inspector) }
        if AnalysisStore.shared.shown && !gripper { out.append(.analysis) }
        if FabStore.shared.shown && !gripper { out.append(.fab) }
        if TuneStore.shared.shown && !gripper { out.append(.tune) }
        if SoundStore.shared.shown && !gripper { out.append(.sound) }
        if model.sketching { out.append(.sketch) }
        if model.measuring { out.append(.measure) }
        if model.section != nil { out.append(.section) }
        if flow.materialsShown { out.append(.materials) }
        if flow.simulationShown && (model.hasPoseStudio || model.canSimulate) { out.append(.simulation) }
        if model.assembly.showsPanel { out.append(.assembly) }
        if model.showsTree { out.append(.navigator) }
        return out
    }

    private var panelKey: String { openCards.map(\.rawValue).joined(separator: ",") }

    private var shownCard: Card? {
        let open = openCards
        if let f = frontCard, open.contains(f) { return f }
        return open.first
    }

    @ViewBuilder private var panelStack: some View {
        let open = openCards
        if let card = shownCard {
            VStack(spacing: 6) {
                if open.count > 1 {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 6) {
                            ForEach(open) { c in
                                Button { withAnimation(Motion.smooth) { frontCard = c } } label: {
                                    Text(c.title).font(.footnote.weight(c == card ? .semibold : .regular))
                                        .padding(.horizontal, 12).frame(height: 30)
                                        .glassCapsule(tint: c == card ? Color.primary.opacity(0.12) : nil)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                    .scrollClipDisabled()
                }
                ScrollView {
                    cardView(card)
                        .overlay(alignment: .topTrailing) {
                            if card != .sketch { closeButton { close(card) } }
                        }
                        .frame(maxWidth: .infinity)
                }
                .scrollBounceBehavior(.basedOnSize)
                .scrollClipDisabled()      // the cards' shadows aren't cut square
                .frame(maxHeight: 360)
                .fixedSize(horizontal: false, vertical: true)
                .id(card)
            }
            .transition(.move(edge: .bottom).combined(with: .opacity))
        }
    }

    @ViewBuilder private func cardView(_ c: Card) -> some View {
        switch c {
        case .inspector:
            if model.source.isGripper { ReceiptLedgerWindow(model: model) } else { ObjectInspectorWindow(model: model) }
        case .analysis: AnalysisPanel(model: model)
        case .fab: FabricatePanel(model: model)
        case .tune: TunePanel(model: model)
        case .sound: RingsAtCard(model: model)
        case .sketch: SketchConstraintPanel(model: model)
        case .measure: MeasurePanel(model: model)
        case .section: SectionPanel(model: model)
        case .materials: PhoneMaterialsPanel(model: model)
        case .simulation:
            if model.hasPoseStudio { PoseStudioWindow(model: model) } else { SimulationWindow(model: model) }
        case .assembly: AssemblyPanel(model: model)
        case .navigator:
            if FabStore.shared.shown { FabJobsNavigator(model: model) } else { DesignModelNavigator(model: model) }
        }
    }

    /// The card's ×: whatever its Mac panel's close would do.
    private func close(_ c: Card) {
        switch c {
        case .inspector: flow.inspectorShown = false
        case .analysis: AnalysisStore.shared.shown = false; AnalysisStore.shared.overlayRevision += 1
        case .fab: FabStore.shared.shown = false
        case .tune: TuneStore.shared.shown = false
        case .sound: SoundStore.shared.malletArmed = false
        case .sketch: break      // the sketch bar's Done / Cancel own leaving a sketch
        case .measure: model.measuring = false
        case .section: model.toggleSmartSection()
        case .materials: flow.materialsShown = false
        case .simulation: flow.simulationShown = false
        case .assembly: model.assembly.showsPanel = false
        case .navigator: model.showsTree = false
        }
    }

    private func closeButton(_ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: "xmark.circle.fill").font(.system(size: 18)).foregroundStyle(.tertiary)
                .frame(width: 36, height: 36).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(4)
        .accessibilityLabel("Close")
    }

    // MARK: bottom: history, playback, intent, the dock

    private var bottomChrome: some View {
        VStack(spacing: 8) {
            if ui.historyOpen && model.usesDocumentTree {
                HistoryFilmstrip(model: model)
                    .padding(.horizontal, 16)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
            if model.timeline != nil { PlaybackBar(model: model).padding(.horizontal, 16) }
            if composerShown {
                HStack(spacing: 6) {
                    ComposerBar(engine: intent, model: model)
                    closeButton { composerShown = false }
                }
                .padding(.horizontal, 16)
            }
            dock
        }
        .padding(.bottom, 4)
        .animation(Motion.smooth, value: ui.historyOpen)
        .animation(Motion.smooth, value: model.sketching)
    }

    /// The Mac dock as-is (compact: Modify and Combine fold into groups),
    /// scrolling sideways when a workspace has more tools than fit.
    private var dock: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Group {
                if model.sketching { SketchBar(model: model) }
                else { GlassDock(model: model, compact: true) }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
        }
        .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
        .defaultScrollAnchor(.center)
        .scrollClipDisabled()
    }

    // MARK: actions

    private func save() {
        if model.documentURL != nil, !model.isLoonDocument {
            model.saveDocument()
            if model.saveError == nil, !model.documentDirty {
                GlassUI.shared.flash("Saved \(model.documentName)", symbol: "checkmark.circle")
            }
        } else {
            saveAsPanel(model)
        }
    }

    private func exportCopy() {
        guard let json = model.documentJSON, let data = DocEdit.serializePretty(json) else { return }
        exportDoc = VcadExportDocument(data: data, contentType: PhoneDocuments.vcadType, filename: model.documentName)
    }

    /// File ▸ Export ▸ STL/STEP/GLB: the kernel writes a temp file off main,
    /// then the system exporter puts it wherever you choose.
    private func export(_ format: DocumentExportFormat) {
        let name = model.documentName.isEmpty ? "part" : model.documentName
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("\(name).\(format.fileExtension)")
        try? FileManager.default.removeItem(at: url)
        guard let json = model.exportableDocumentData() else {
            if format == .stl, model.exportSTL(to: url), let data = try? Data(contentsOf: url) {
                exportDoc = VcadExportDocument(data: data, contentType: format.contentType, filename: name)
            } else {
                GlassUI.shared.flash("Nothing to export as \(format.label)", symbol: "exclamationmark.triangle")
            }
            return
        }
        let base = model.exportBaseDir
        GlassUI.shared.flash("Exporting \(format.label)…", symbol: "square.and.arrow.up")
        Task.detached(priority: .userInitiated) {
            let result = KernelExport.export(json: json, baseDir: base, to: url, format: format)
            let data = try? Data(contentsOf: url)
            await MainActor.run {
                switch result {
                case .success where data != nil:
                    exportDoc = VcadExportDocument(data: data!, contentType: format.contentType, filename: name)
                case .failure(let e):
                    GlassUI.shared.flash("Export failed: \(e.message)", symbol: "exclamationmark.triangle")
                default:
                    GlassUI.shared.flash("Export failed", symbol: "exclamationmark.triangle")
                }
            }
        }
    }

    private var exportShown: Binding<Bool> {
        Binding(get: { exportDoc != nil }, set: { if !$0 { exportDoc = nil } })
    }

    // MARK: long-press context (the Mac's right-click menu)

    private var contextShown: Binding<Bool> {
        Binding(get: { contextPart != nil }, set: { if !$0 { contextPart = nil } })
    }

    private var contextTitle: String {
        guard let pi = contextPart else { return "" }
        return model.featureNodes.first(where: { $0.partIndex == pi })?.name ?? "Part \(pi + 1)"
    }

    @ViewBuilder
    private func contextActions(_ pi: Int) -> some View {
        Button("Inspect") { flow.inspectorShown = true }
        Button("Frame") { ReleaseWindowController.shared.frame(entityNamed: "part\(pi)") }
        Button(model.isolatedPart == pi ? "Exit Isolate" : "Isolate") { model.isolate(part: pi) }
        Button(model.isPartVisible(pi) ? "Hide" : "Show") { model.toggleVisibility(part: pi) }
        Button("Material…") { flow.materialsShown = true }
        if let fid = model.featureNodes.first(where: { $0.partIndex == pi })?.id {
            Button("Add to Selection") { model.toggleMultiSelect(part: pi, featureID: fid) }
        }
        Button("Delete", role: .destructive) { model.deletePart(pi) }
    }
}

/// Material presets for the selected part, as a tappable swatch grid — the
/// Mac's right-click ▸ Material menu, as a card.
struct PhoneMaterialsPanel: View {
    @Bindable var model: EditorModel

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            PanelHeader(title: "Material")
            if let pi = model.selectedPartIndex {
                let current = model.materialName(forPart: pi) ?? "default"
                ForEach(MaterialPreset.grouped, id: \.category) { group in
                    VStack(alignment: .leading, spacing: 8) {
                        Eyebrow(group.category.capitalized)
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 64), spacing: 8)], spacing: 8) {
                            ForEach(group.items) { p in
                                Button { model.setPartMaterial(pi, p.key) } label: {
                                    VStack(spacing: 4) {
                                        Circle().fill(Color(uiColor: p.nsColor))
                                            .frame(width: 32, height: 32)
                                            .overlay(Circle().strokeBorder(p.key == current ? Color.accentColor : .clear,
                                                                           lineWidth: 2.5).padding(-3))
                                        Text(p.name).font(.caption2).lineLimit(1)
                                    }
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                }
            } else {
                Text("Tap a part to give it a material.").font(.callout).foregroundStyle(.secondary)
            }
        }
        .padding(Theme.Space.m).frame(width: Theme.Width.inspector).panelSurface()
    }
}
