import SwiftUI
import QuickLook

/// The phone layout: the viewport fills the screen; a slim top bar carries
/// the document, undo/redo and save; the bottom carries the intent bar and
/// four panels (Features, Inspect, Materials, Simulate) that rise as a sheet
/// over the viewport — which stays live and touchable behind it.
struct PhoneRootView: View {
    @Bindable var model: EditorModel
    @Bindable var intent: IntentEngine
    @Bindable private var flow = PhoneDocumentFlow.shared
    @State private var detent: PresentationDetent = .fraction(0.42)
    @State private var contextPart: Int?
    @State private var exportDoc: VcadExportDocument?

    var body: some View {
        ZStack(alignment: .top) {
            PhoneViewport(model: model, onLongPress: { contextPart = $0 })
            VStack(spacing: 8) {
                topBar
                ToastHost()
                if model.solving || model.loading?.isEvaluating == true {
                    ProgressView().controlSize(.small).padding(6).glassCapsule()
                }
                Spacer()
            }
            .padding(.horizontal, 12)
        }
        .safeAreaInset(edge: .bottom) { bottomBar }
        .sheet(item: $flow.panel) { p in
            NavigationStack {
                ScrollView { panelContent(p).padding(16) }
                    .navigationTitle(p.title)
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar {
                        ToolbarItem(placement: .topBarTrailing) {
                            Button("Done") { flow.panel = nil }
                        }
                    }
            }
            .presentationDetents([.fraction(0.42), .large], selection: $detent)
            .presentationBackgroundInteraction(.enabled(upThrough: .fraction(0.42)))
            .presentationContentInteraction(.scrolls)
        }
        .confirmationDialog(contextTitle, isPresented: contextShown, titleVisibility: .visible) {
            if let pi = contextPart { contextActions(pi) }
        }
        .fileImporter(isPresented: $flow.importing, allowedContentTypes: PhoneDocuments.openableTypes) { result in
            if case let .success(url) = result { PhoneDocuments.open(url, into: model) }
        }
        .fileExporter(isPresented: exportShown, document: exportDoc, contentType: PhoneDocuments.vcadType,
                      defaultFilename: model.documentName) { result in
            if case .success = result { GlassUI.shared.flash("Exported a copy", symbol: "square.and.arrow.up") }
            exportDoc = nil
        }
        .quickLookPreview($flow.quickLookURL)
        .alert("Couldn’t save", isPresented: Binding(get: { model.saveError != nil },
                                                     set: { if !$0 { model.saveError = nil } })) {
            Button("OK") { model.saveError = nil }
        } message: { Text(model.saveError ?? "") }
    }

    // MARK: top bar

    private var topBar: some View {
        HStack(spacing: 6) {
            Menu {
                Button { flow.importing = true } label: { Label("Open from Files…", systemImage: "folder") }
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
            Spacer(minLength: 4)
            barButton("arrow.uturn.backward", "Undo", enabled: model.canUndo) { model.undo() }
            barButton("arrow.uturn.forward", "Redo", enabled: model.canRedo) { model.redo() }
            Menu {
                Button { save() } label: { Label("Save", systemImage: "square.and.arrow.down") }
                Button { exportCopy() } label: { Label("Export a Copy…", systemImage: "square.and.arrow.up") }
                    .disabled(model.documentJSON == nil)
                Button { exportUSDZPanel(model) } label: { Label("View in AR", systemImage: "arkit") }
                    .disabled(!model.canExport)
                Divider()
                Button { model.resetCamera(animated: true) } label: { Label("Frame All", systemImage: "viewfinder") }
            } label: {
                Image(systemName: "ellipsis").font(.system(size: 16, weight: .semibold))
                    .frame(width: 36, height: 36).glassCapsule()
            }
            .accessibilityIdentifier("fileMenu")
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

    // MARK: bottom bar

    private var bottomBar: some View {
        VStack(spacing: 8) {
            if model.timeline != nil { PlaybackBar(model: model).padding(.horizontal, 12) }
            ComposerBar(engine: intent, model: model)
                .padding(.horizontal, 12)
            HStack(spacing: 0) {
                ForEach(PhonePanel.allCases) { p in
                    Button {
                        if flow.panel == p { flow.panel = nil } else { detent = .fraction(0.42); flow.panel = p }
                    } label: {
                        VStack(spacing: 3) {
                            Image(systemName: p.symbol).font(.system(size: 18))
                            Text(p.title).font(.caption2)
                        }
                        .frame(maxWidth: .infinity).padding(.vertical, 6)
                        .foregroundStyle(flow.panel == p ? Color.accentColor : .primary)
                    }
                    .accessibilityIdentifier("panel.\(p.rawValue)")
                }
            }
            .padding(.horizontal, 8)
        }
        .padding(.top, 8)
        .background(.bar)
    }

    @ViewBuilder
    private func panelContent(_ p: PhonePanel) -> some View {
        switch p {
        case .features:
            VStack(alignment: .leading, spacing: 14) {
                ToolPaletteView(model: model)
                FeatureTreeView(model: model, embedded: true)
            }
        case .inspect:
            InspectorView(model: model)
        case .materials:
            PhoneMaterialsPanel(model: model)
        case .simulate:
            if model.canSimulate {
                SimInspector(model: model)
            } else {
                ContentUnavailableView("Nothing to simulate", systemImage: "figure.walk.motion",
                                       description: Text("Open an assembly with joints to run its physics."))
            }
        }
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
        exportDoc = VcadExportDocument(data: data)
    }

    private var exportShown: Binding<Bool> {
        Binding(get: { exportDoc != nil }, set: { if !$0 { exportDoc = nil } })
    }

    // MARK: long-press context

    private var contextShown: Binding<Bool> {
        Binding(get: { contextPart != nil }, set: { if !$0 { contextPart = nil } })
    }

    private var contextTitle: String {
        guard let pi = contextPart else { return "" }
        return model.featureNodes.first(where: { $0.partIndex == pi })?.name ?? "Part \(pi + 1)"
    }

    @ViewBuilder
    private func contextActions(_ pi: Int) -> some View {
        Button("Edit Parameters") { detent = .fraction(0.42); flow.panel = .inspect }
        Button("Frame") { ReleaseWindowController.shared.frame(entityNamed: "part\(pi)") }
        Button(model.isolatedPart == pi ? "Show All" : "Isolate") { model.isolate(part: pi) }
        Button("Hide") { model.toggleVisibility(part: pi) }
        if let fid = model.featureNodes.first(where: { $0.partIndex == pi })?.id {
            Button("Add to Selection") { model.toggleMultiSelect(part: pi, featureID: fid) }
        }
        Button("Delete", role: .destructive) { model.deletePart(pi) }
    }
}

enum PhonePanel: String, CaseIterable, Identifiable {
    case features, inspect, materials, simulate
    var id: String { rawValue }
    var title: String {
        switch self {
        case .features: "Features"
        case .inspect: "Inspect"
        case .materials: "Materials"
        case .simulate: "Simulate"
        }
    }
    var symbol: String {
        switch self {
        case .features: "list.bullet.indent"
        case .inspect: "slider.horizontal.3"
        case .materials: "paintpalette"
        case .simulate: "play.circle"
        }
    }
}

/// Material presets for the selected part, as a tappable swatch grid.
struct PhoneMaterialsPanel: View {
    @Bindable var model: EditorModel

    var body: some View {
        if let pi = model.selectedPartIndex {
            let current = model.materialName(forPart: pi) ?? "default"
            VStack(alignment: .leading, spacing: 16) {
                ForEach(MaterialPreset.grouped, id: \.category) { group in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(group.category.capitalized).font(.footnote.weight(.semibold)).foregroundStyle(.secondary)
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 76), spacing: 10)], spacing: 10) {
                            ForEach(group.items) { p in
                                Button { model.setPartMaterial(pi, p.key) } label: {
                                    VStack(spacing: 5) {
                                        Circle().fill(Color(uiColor: p.nsColor))
                                            .frame(width: 38, height: 38)
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
            }
        } else {
            ContentUnavailableView("Select a part", systemImage: "hand.tap",
                                   description: Text("Tap a part in the viewport to give it a material."))
        }
    }
}
