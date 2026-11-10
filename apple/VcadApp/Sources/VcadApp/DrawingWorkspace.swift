#if os(macOS)
import AppKit
import CVcadFFI
import Foundation
import SwiftUI
import UniformTypeIdentifiers

// The Drawing workspace: 2D shop drawings on vcad-kernel-drafting, through
// vcad-ffi's `vcad_drawing_*` (crates/vcad-ffi/src/drafting.rs).
//
// The kernel composes the whole A4 sheet — main view, overall dimensions,
// section and detail views, GD&T frames, title block, BOM — with the web
// app's PDF layout, and hands back its display list. This view draws that
// list as-is, so the sheet on screen is the PDF File ▸ Export Drawing writes.
// DXF goes through the web's own writer (`projected_view_to_dxf`).
//
// Title block, section lines and the BOM toggle persist on the document (the
// engine's `drawing-settings` feature → IR `drawing`), so a sheet authored on
// the web opens here unchanged and vice versa. View, details and GD&T are
// session state, as on the web.

// MARK: - Kernel wire types

/// A point in sheet or view mm.
struct DrawingPoint: Codable, Equatable { var x: Double; var y: Double }

/// The composed sheet (`DrawingResult` in drafting.rs).
struct DrawingSheetResult: Decodable {
    struct Line: Decodable { let `class`: String; let start: DrawingPoint; let end: DrawingPoint }
    struct Arc: Decodable {
        struct Geometry: Decodable { let center: DrawingPoint; let radius: Double; let start_angle: Double; let end_angle: Double }
        let `class`: String
        let arc: Geometry
    }
    struct Polygon: Decodable { let `class`: String; let points: [DrawingPoint] }
    struct Text: Decodable {
        let position: DrawingPoint
        let text: String
        let height: Double
        let rotation: Double
        let alignment: String
    }
    struct Sheet: Decodable { let lines: [Line]; let arcs: [Arc]; let polygons: [Polygon]; let texts: [Text] }
    struct Bounds: Decodable { let min_x: Double; let min_y: Double; let max_x: Double; let max_y: Double }
    struct Placement: Decodable {
        let sheet_center: [Double]
        let view_center: [Double]
        let scale: Double
        let scale_note: String
        let bounds: Bounds

        /// Sheet mm → main-view mm (where details, sections and GD&T anchor).
        func toView(_ p: DrawingPoint) -> DrawingPoint {
            DrawingPoint(x: view_center[0] + (p.x - sheet_center[0]) / scale,
                         y: view_center[1] + (p.y - sheet_center[1]) / scale)
        }
    }
    let width: Double
    let height: Double
    let sheet: Sheet
    let main: Placement
    let visible_edges: Int
    let hidden_edges: Int
    let dxf_entities: Int
    let sections: Int
}

/// IR `DrawingTitleBlock` (camelCase, as persisted in `.vcad`).
struct DrawingTitleBlock: Codable, Equatable {
    var partName = ""
    var author = ""
    var date = ""
    var scale = ""
    var material = ""
    var revision = "A"
}

/// IR `DrawingSectionLine`.
struct DrawingSectionLine: Codable, Equatable, Identifiable {
    var id: String
    var view: String
    var label: String
    var points: [[Double]]
}

/// IR `DrawingSettings`.
struct DrawingSettingsIR: Codable, Equatable {
    var titleBlock: DrawingTitleBlock?
    var sections: [DrawingSectionLine]?
    var showBom: Bool?
}

struct DrawingDetail: Codable, Equatable, Identifiable {
    var id: String { label }
    var label: String
    var center: [Double]
    var scale: Double = 2
    var width: Double
    var height: Double
}

enum GdtKind: String, Codable, CaseIterable, Identifiable {
    case flatness, position, perpendicularity, datum
    var id: String { rawValue }
    var label: String {
        switch self {
        case .flatness: "Flatness ⏥"
        case .position: "Position ⌖"
        case .perpendicularity: "Perpendicularity ⟂"
        case .datum: "Datum Feature"
        }
    }
}

struct DrawingGdt: Codable, Equatable, Identifiable {
    var id = UUID()
    var kind: GdtKind
    var tolerance: Double
    var datum: String?
    var at: [Double]
    private enum CodingKeys: String, CodingKey { case kind, tolerance, datum, at }
}

/// `DrawingRequest` in drafting.rs.
private struct DrawingRequestWire: Encodable {
    var view: String
    var show_hidden: Bool
    var show_dimensions: Bool
    var details: [DrawingDetail]
    var gdt: [DrawingGdt]
    var drawing: DrawingSettingsIR?
    var base_dir: String?
}

enum DrawingView: String, CaseIterable, Identifiable {
    case front, back, top, bottom, left, right, isometric
    var id: String { rawValue }
    var label: String { rawValue.capitalized }
    var canSection: Bool { self != .isometric }
}

enum DrawingExportFormat {
    case dxf, pdf
    var label: String { self == .dxf ? "DXF" : "PDF" }
    var fileExtension: String { self == .dxf ? "dxf" : "pdf" }
    var contentType: UTType { self == .dxf ? (UTType(filenameExtension: "dxf") ?? .data) : .pdf }
    var ffiCode: UInt32 { self == .dxf ? UInt32(VCAD_DRAWING_DXF) : UInt32(VCAD_DRAWING_PDF) }
}

// MARK: - Kernel calls

enum DrawingKernel {
    /// Compose the sheet. Blocking — call off the main thread.
    nonisolated static func sheet(document: Data, request: Data) -> Result<DrawingSheetResult, KernelError> {
        let doc = String(decoding: document, as: UTF8.self)
        let req = String(decoding: request, as: UTF8.self)
        guard let p = doc.withCString({ d in req.withCString { r in vcad_drawing_sheet_json(d, r) } }) else {
            return .failure(KernelError(vcadLastError("Couldn't compose the drawing")))
        }
        defer { vcad_string_free(p) }
        let json = Data(bytes: p, count: strlen(p))
        do { return .success(try JSONDecoder().decode(DrawingSheetResult.self, from: json)) } catch {
            return .failure(KernelError("drawing: \(error)"))
        }
    }

    /// Write DXF/PDF. Returns DXF entities / PDF bytes.
    nonisolated static func export(document: Data, request: Data, to url: URL,
                                   format: DrawingExportFormat) -> Result<Int, KernelError> {
        let doc = String(decoding: document, as: UTF8.self)
        let req = String(decoding: request, as: UTF8.self)
        let n = doc.withCString { d in req.withCString { r in url.path.withCString { o in
            vcad_drawing_export(d, r, o, format.ffiCode)
        } } }
        return n >= 0 ? .success(Int(n)) : .failure(KernelError(vcadLastError("\(format.label) export failed")))
    }
}

// MARK: - State

/// What a click on the sheet does.
enum DrawingTool: Hashable {
    case select, detail, section, gdt(GdtKind)
    var hint: String? {
        switch self {
        case .select: nil
        case .detail: "Click the region to magnify"
        case .section: "Click where the cutting plane crosses the view"
        case .gdt(.datum): "Click to place the datum feature symbol"
        case .gdt: "Click to place the feature control frame"
        }
    }
}

@MainActor @Observable
final class DrawingStore {
    static let shared = DrawingStore()

    var view: DrawingView = .front { didSet { if view != oldValue { zoom = 1; pan = .zero; refresh() } } }
    var showHidden = true { didSet { refresh() } }
    var showDimensions = true { didSet { refresh() } }
    var details: [DrawingDetail] = [] { didSet { refresh() } }
    var gdt: [DrawingGdt] = [] { didSet { refresh() } }
    var tool: DrawingTool = .select
    var gdtTolerance = 0.05
    var gdtDatum = "A"
    var zoom: CGFloat = 1
    var pan: CGSize = .zero

    /// Drawing settings as edited here; nil = whatever the document carries.
    private(set) var settings: DrawingSettingsIR?
    private(set) var result: DrawingSheetResult?
    private(set) var error: String?
    private(set) var busy = false

    @ObservationIgnored weak var model: EditorModel?
    /// The `drawing-settings` feature this session created (updated in place
    /// after the first edit so the history doesn't fill with copies).
    @ObservationIgnored private var settingsFeatureId: String?
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var lastDocument: Data?

    /// The document's persisted settings, read from the materialized IR.
    var documentSettings: DrawingSettingsIR {
        if let settings { return settings }
        guard let raw = model?.documentJSON?["drawing"],
              let data = try? JSONSerialization.data(withJSONObject: raw),
              let s = try? JSONDecoder().decode(DrawingSettingsIR.self, from: data) else { return DrawingSettingsIR() }
        return s
    }

    var sectionsForView: [DrawingSectionLine] { (documentSettings.sections ?? []).filter { $0.view == view.rawValue } }

    private func requestData() -> Data {
        let wire = DrawingRequestWire(view: view.rawValue, show_hidden: showHidden, show_dimensions: showDimensions,
                                      details: details, gdt: gdt, drawing: settings,
                                      base_dir: model?.exportBaseDir)
        return (try? JSONEncoder().encode(wire)) ?? Data("{}".utf8)
    }

    /// Recompose off the main thread; a newer request supersedes an older one.
    func refresh(force: Bool = false) {
        guard let model, model.workspace == .drawing else { return }
        guard let doc = model.exportableDocumentData() else {
            result = nil
            error = "Open a .vcad document to make a drawing of it."
            return
        }
        if !force, doc == lastDocument, busy == false, result != nil, pendingRequest == requestData() { return }
        lastDocument = doc
        let req = requestData()
        pendingRequest = req
        generation += 1
        let gen = generation
        busy = true
        Task.detached(priority: .userInitiated) {
            let r = DrawingKernel.sheet(document: doc, request: req)
            await MainActor.run {
                guard gen == self.generation else { return }
                self.busy = false
                switch r {
                case let .success(s): self.result = s; self.error = nil
                case let .failure(e): self.error = e.message
                }
            }
        }
    }
    @ObservationIgnored private var pendingRequest: Data?

    // MARK: edits

    /// Persist drawing settings on the document as one undo step.
    func updateSettings(name: String, _ mutate: (inout DrawingSettingsIR) -> Void) {
        var s = documentSettings
        mutate(&s)
        settings = s
        if let model, model.canEditDocument || model.agentCanEdit {
            let enc = JSONEncoder()
            func str<T: Encodable>(_ v: T?) -> String? { v.flatMap { try? enc.encode($0) }.map { String(decoding: $0, as: UTF8.self) } }
            let feature = FeatureInput.drawingSettings(titleBlock: str(s.titleBlock), sections: str(s.sections),
                                                       showBom: str(s.showBom))
            let known = settingsFeatureId
            if let r = try? AgentEdits.edit(model, name: name, { engine in
                if let known, let r = try? engine.apply(.updateFeature(id: known, feature: feature)) { return r }
                return try engine.apply(.addFeature(feature: feature, name: "Drawing"))
            }), let id = r.createdFeatureId {
                settingsFeatureId = id
            }
            // The materialized document now carries the settings.
            settings = nil
            if documentSettings != s { settings = s }
        }
        refresh(force: true)
    }

    func addSection(atViewX x: Double) {
        guard view.canSection, let b = result?.main.bounds else { return }
        let used = Set((documentSettings.sections ?? []).map(\.label))
        let label = nextLetter(excluding: used)
        let pad = max(b.max_y - b.min_y, 1) * 0.1
        let line = DrawingSectionLine(id: "section-\(UUID().uuidString.prefix(8))", view: view.rawValue, label: label,
                                      points: [[x, b.min_y - pad], [x, b.max_y + pad]])
        updateSettings(name: "Add Section \(label)-\(label)") { $0.sections = ($0.sections ?? []) + [line] }
    }

    func clearSections() {
        updateSettings(name: "Clear Sections") { s in s.sections = (s.sections ?? []).filter { $0.view != view.rawValue } }
    }

    func addDetail(at p: DrawingPoint) {
        guard let b = result?.main.bounds else { return }
        let size = max(min(b.max_x - b.min_x, b.max_y - b.min_y) * 0.3, 1)
        let used = Set(details.map(\.label) + (documentSettings.sections ?? []).map(\.label))
        details.append(DrawingDetail(label: nextLetter(excluding: used), center: [p.x, p.y], width: size, height: size))
    }

    func addGdt(_ kind: GdtKind, at p: DrawingPoint) {
        gdt.append(DrawingGdt(kind: kind, tolerance: gdtTolerance,
                              datum: gdtDatum.isEmpty ? nil : String(gdtDatum.prefix(1)).uppercased(),
                              at: [p.x, p.y]))
    }

    private func nextLetter(excluding used: Set<String>) -> String {
        for c in "ABCDEFGHJKLMNPRSTUVWXYZ" where !used.contains(String(c)) { return String(c) }
        return "Z"
    }

    // MARK: export

    func export(_ format: DrawingExportFormat) {
        guard let model, let doc = model.exportableDocumentData() else {
            presentDrawingAlert("Nothing to export", "Open a .vcad document first.")
            return
        }
        let panel = NSSavePanel()
        panel.allowedContentTypes = [format.contentType]
        panel.nameFieldStringValue = "\(model.documentName)-\(view.rawValue).\(format.fileExtension)"
        panel.prompt = "Export"
        panel.canCreateDirectories = true
        let req = requestData()
        present(panel) {
            guard let url = panel.url else { return }
            Task.detached(priority: .userInitiated) {
                let r = DrawingKernel.export(document: doc, request: req, to: url, format: format)
                await MainActor.run {
                    if case let .failure(e) = r { presentDrawingAlert("\(format.label) export failed", e.message) }
                }
            }
        }
    }
}

@MainActor private func presentDrawingAlert(_ title: String, _ message: String) {
    let alert = NSAlert()
    alert.alertStyle = .warning
    alert.messageText = title
    alert.informativeText = message
    if let host = ReleaseWindowController.shared.hostWindow, host.styleMask.contains(.titled) {
        alert.beginSheetModal(for: host)
    } else {
        alert.runModal()
    }
}

// MARK: - Workspace view

struct DrawingWorkspaceView: View {
    @Bindable var model: EditorModel
    @Bindable private var store = DrawingStore.shared
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        ZStack(alignment: .top) {
            Color(nsColor: .underPageBackgroundColor)
            Group {
                if let r = store.result {
                    DrawingSheetCanvas(result: r, store: store)
                } else if let e = store.error {
                    ContentUnavailableView("No Drawing", systemImage: "ruler", description: Text(e))
                } else {
                    ProgressView("Projecting views…")
                }
            }
            HStack(alignment: .top, spacing: 0) {
                if model.showsTree {
                    DrawingNavigator(store: store).modifier(ChromeRegion(key: "drawingNavigator"))
                }
                Spacer(minLength: 0)
                if model.showsInspector {
                    DrawingInspector(store: store).modifier(ChromeRegion(key: "drawingInspector"))
                }
            }
            .padding(.horizontal, Glass.inset).padding(.top, Glass.toolbarHeight)
            // One status pill: the active tool's hint, else the sheet's counts.
            VStack {
                Spacer()
                DrawingStatusPill(store: store).padding(.bottom, Glass.inset + 6)
            }
        }
        .animation(Motion.panel, value: model.showsTree)
        .animation(Motion.panel, value: model.showsInspector)
        .onAppear { store.model = model; store.refresh(force: true) }
        .onChange(of: model.source) { _, _ in store.details = []; store.gdt = []; store.refresh(force: true) }
        .onChange(of: model.undoStack.count) { _, _ in store.refresh() }
        .onChange(of: model.redoStack.count) { _, _ in store.refresh() }
    }
}

/// Left panel: the sheet's views and annotations.
private struct DrawingNavigator: View {
    @Bindable var store: DrawingStore
    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            PanelHeader(title: "Sheet")
            Divider()
            Eyebrow("Views")
            ForEach(DrawingView.allCases) { v in
                Button { store.view = v } label: {
                    HStack {
                        Text(v.label)
                        Spacer(minLength: 0)
                        if v == store.view, let r = store.result {
                            Text(r.main.scale_note).foregroundStyle(.secondary).monospacedDigit()
                        }
                    }
                    .font(.callout).padding(.horizontal, 6).padding(.vertical, 3)
                    .contentShape(Rectangle())
                    .selectableRow(selected: v == store.view)
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(v == store.view ? .isSelected : [])
            }
            if !store.sectionsForView.isEmpty || !store.details.isEmpty || !store.gdt.isEmpty {
                Divider()
                Eyebrow("Annotations")
                ForEach(store.sectionsForView, id: \.label) { s in
                    Label("Section \(s.label)–\(s.label)", systemImage: "rectangle.split.2x1").font(.callout)
                }
                ForEach(store.details) { d in
                    Label("Detail \(d.label)", systemImage: "plus.magnifyingglass").font(.callout)
                }
                if !store.gdt.isEmpty {
                    Label("\(store.gdt.count) GD&T frame\(store.gdt.count == 1 ? "" : "s")", systemImage: "a.square").font(.callout)
                }
            }
        }
        .padding(Theme.Space.m).frame(width: Theme.Width.navigator).panelSurface()
    }
}

/// Right panel: the selected view's display options, the parts list and the
/// title block.
private struct DrawingInspector: View {
    @Bindable var store: DrawingStore
    @State private var titleBlockShown = false
    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            PanelHeader(title: "\(store.view.label) View")
            Divider()
            Toggle("Hidden lines", isOn: $store.showHidden).font(.callout)
            Toggle("Dimensions", isOn: $store.showDimensions).font(.callout)
            Toggle("Parts list", isOn: Binding(get: { store.documentSettings.showBom ?? false },
                                               set: { v in store.updateSettings(name: v ? "Show BOM" : "Hide BOM") { $0.showBom = v } }))
                .font(.callout)
            if case .gdt = store.tool {
                Divider()
                Eyebrow("GD&T")
                HStack(spacing: 4) {
                    Text("Tolerance").font(.callout).foregroundStyle(.secondary)
                    Spacer(minLength: 2)
                    TextField("Tolerance", value: $store.gdtTolerance, format: .number)
                        .textFieldStyle(.roundedBorder).frame(width: 52).multilineTextAlignment(.trailing)
                    Text("mm").font(.callout).foregroundStyle(.secondary)
                }
                HStack {
                    Text("Datum").font(.callout).foregroundStyle(.secondary)
                    Spacer()
                    TextField("Datum", text: $store.gdtDatum).textFieldStyle(.roundedBorder).frame(width: 34)
                }
                Picker("Frame", selection: $store.tool) {
                    ForEach(GdtKind.allCases) { k in Text(k.label).tag(DrawingTool.gdt(k)) }
                }.labelsHidden()
            }
            Divider()
            Button("Title Block…") { titleBlockShown.toggle() }
                .popover(isPresented: $titleBlockShown, arrowEdge: .leading) {
                    DrawingTitleBlockEditor(store: store, shown: $titleBlockShown)
                }
            Menu("Clear") {
                Button("Clear Details") { store.details = [] }.disabled(store.details.isEmpty)
                Button("Clear Sections in This View") { store.clearSections() }.disabled(store.sectionsForView.isEmpty)
                Button("Clear GD&T") { store.gdt = [] }.disabled(store.gdt.isEmpty)
            }.fixedSize()
        }
        .controlSize(.small)
        .padding(Theme.Space.m).frame(width: Theme.Width.inspector).panelSurface()
    }
}

/// The tool's hint while one is armed; otherwise the sheet's numbers.
private struct DrawingStatusPill: View {
    let store: DrawingStore
    var body: some View {
        HStack(spacing: 8) {
            if store.busy { ProgressView().controlSize(.mini) }
            if let hint = store.tool.hint {
                Label(hint, systemImage: "hand.point.up.left")
            } else if let e = store.error, store.result != nil {
                Label(e, systemImage: "exclamationmark.triangle")
            } else if let r = store.result {
                Text("\(r.main.scale_note) · \(r.visible_edges) visible · \(r.hidden_edges) hidden edges · DXF \(r.dxf_entities)")
                    .monospacedDigit().foregroundStyle(.secondary)
            }
        }
        .font(.callout)
        .padding(.horizontal, 12).padding(.vertical, 6)
        .glassCapsule()
        .modifier(ChromeRegion(key: "drawingStatus"))
    }
}

/// Title block fields, persisted on the document (same fields as the web's
/// TitleBlockEditor).
struct DrawingTitleBlockEditor: View {
    let store: DrawingStore
    @Binding var shown: Bool
    @State private var fields = DrawingTitleBlock()

    private static let rows: [(String, WritableKeyPath<DrawingTitleBlock, String>, String)] = [
        ("Part name", \.partName, "Bracket, rev A"),
        ("Drawn by", \.author, "Your name"),
        ("Date", \.date, "2026-07-18"),
        ("Scale", \.scale, "1:1"),
        ("Material", \.material, "6061-T6 AL"),
        ("Revision", \.revision, "A"),
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            Text("Title Block").font(.headline)
            Form {
                ForEach(Self.rows, id: \.0) { row in
                    TextField(row.0, text: Binding(get: { fields[keyPath: row.1] }, set: { fields[keyPath: row.1] = $0 }),
                              prompt: Text(row.2))
                }
            }.formStyle(.columns)
            HStack {
                Spacer()
                Button("Cancel") { shown = false }.keyboardShortcut(.cancelAction)
                Button("Apply") {
                    let f = fields
                    store.updateSettings(name: "Edit Title Block") { $0.titleBlock = f }
                    shown = false
                }.keyboardShortcut(.defaultAction)
            }
        }
        .padding(Theme.Space.l).frame(width: 300)
        .onAppear {
            var f = store.documentSettings.titleBlock ?? DrawingTitleBlock()
            if f.date.isEmpty { f.date = String(ISO8601DateFormatter().string(from: Date()).prefix(10)) }
            fields = f
        }
    }
}

// MARK: - Sheet canvas

/// Draws the kernel's sheet display list. Pinch/scroll to zoom, drag to pan,
/// click to place with the armed tool.
struct DrawingSheetCanvas: View {
    let result: DrawingSheetResult
    @Bindable var store: DrawingStore
    @Environment(\.colorScheme) private var scheme
    @State private var dragStart: CGSize?
    @State private var zoomStart: CGFloat?

    private func frame(in size: CGSize) -> (fit: CGFloat, origin: CGPoint) {
        let margin: CGFloat = 24
        let fit = min((size.width - 2 * margin) / result.width, (size.height - 2 * margin) / result.height) * store.zoom
        let w = result.width * fit, h = result.height * fit
        return (fit, CGPoint(x: (size.width - w) / 2 + store.pan.width, y: (size.height - h) / 2 + store.pan.height))
    }

    var body: some View {
        GeometryReader { geo in
            let f = frame(in: geo.size)
            Canvas { ctx, _ in
                draw(ctx: &ctx, fit: f.fit, origin: f.origin)
            }
            .contentShape(Rectangle())
            .gesture(DragGesture(minimumDistance: 4)
                .onChanged { v in
                    if dragStart == nil { dragStart = store.pan }
                    store.pan = CGSize(width: dragStart!.width + v.translation.width,
                                       height: dragStart!.height + v.translation.height)
                }
                .onEnded { _ in dragStart = nil })
            .simultaneousGesture(MagnifyGesture()
                .onChanged { v in
                    if zoomStart == nil { zoomStart = store.zoom }
                    store.zoom = min(max(zoomStart! * v.magnification, 0.25), 12)
                }
                .onEnded { _ in zoomStart = nil })
            .onTapGesture(count: 2) { store.zoom = 1; store.pan = .zero }
            .onTapGesture { location in click(location, fit: f.fit, origin: f.origin) }
            .accessibilityLabel("Drawing sheet, \(result.main.scale_note)")
        }
    }

    private func click(_ p: CGPoint, fit: CGFloat, origin: CGPoint) {
        let sheet = DrawingPoint(x: Double((p.x - origin.x) / fit), y: result.height - Double((p.y - origin.y) / fit))
        let v = result.main.toView(sheet)
        switch store.tool {
        case .select: return
        case .detail: store.addDetail(at: v)
        case .section: store.addSection(atViewX: v.x)
        case let .gdt(kind): store.addGdt(kind, at: v)
        }
        store.tool = .select
    }

    private var ink: Color { scheme == .dark ? Color(white: 0.92) : Color(red: 0.08, green: 0.12, blue: 0.22) }
    private var paper: Color { scheme == .dark ? Color(white: 0.13) : .white }

    private func style(_ cls: String, fit: CGFloat) -> (Color, StrokeStyle) {
        let mm = { (w: Double) in max(CGFloat(w) * fit, 0.5) }
        switch cls {
        case "visible", "section": return (ink, StrokeStyle(lineWidth: mm(0.5), lineCap: .round))
        case "hidden": return (ink.opacity(0.7), StrokeStyle(lineWidth: mm(0.25), dash: [3 * fit, 1.5 * fit]))
        case "hatch": return (ink.opacity(0.55), StrokeStyle(lineWidth: mm(0.13)))
        case "dimension": return (Color.primary.opacity(0.75), StrokeStyle(lineWidth: mm(0.18)))
        case "cuttingplane": return (ink, StrokeStyle(lineWidth: mm(0.5), dash: [8 * fit, 1.5 * fit, 1.5 * fit, 1.5 * fit]))
        case "center": return (ink.opacity(0.7), StrokeStyle(lineWidth: mm(0.18), dash: [8 * fit, 1.5 * fit, 1.5 * fit, 1.5 * fit]))
        case "border": return (ink, StrokeStyle(lineWidth: mm(0.7)))
        default: return (ink, StrokeStyle(lineWidth: mm(0.25)))
        }
    }

    private func draw(ctx: inout GraphicsContext, fit: CGFloat, origin: CGPoint) {
        let H = result.height
        func pt(_ p: DrawingPoint) -> CGPoint { CGPoint(x: origin.x + p.x * fit, y: origin.y + (H - p.y) * fit) }
        let page = CGRect(x: origin.x, y: origin.y, width: result.width * fit, height: H * fit)
        ctx.drawLayer { l in
            l.addFilter(.shadow(color: .black.opacity(0.18), radius: 8, y: 2))
            l.fill(Path(page), with: .color(paper))
        }

        // Batch lines per class: one stroke call per style.
        var byClass: [String: Path] = [:]
        for line in result.sheet.lines {
            byClass[line.class, default: Path()].move(to: pt(line.start))
            byClass[line.class]!.addLine(to: pt(line.end))
        }
        for arc in result.sheet.arcs {
            let a = arc.arc
            var p = byClass[arc.class, default: Path()]
            // Sheet y is up, screen y is down: negate angles and flip winding.
            p.addArc(center: pt(a.center), radius: a.radius * fit,
                     startAngle: .radians(-a.start_angle), endAngle: .radians(-a.end_angle), clockwise: true)
            byClass[arc.class] = p
        }
        for (cls, path) in byClass {
            let (color, stroke) = style(cls, fit: fit)
            ctx.stroke(path, with: .color(color), style: stroke)
        }
        for poly in result.sheet.polygons where poly.points.count > 2 {
            var p = Path()
            p.addLines(poly.points.map(pt))
            p.closeSubpath()
            ctx.fill(p, with: .color(style(poly.class, fit: fit).0))
        }
        for t in result.sheet.texts where !t.text.isEmpty {
            // Drafting text height is cap height; a font's point size is ~1.4×.
            let size = max(t.height * fit * 1.4, 4)
            let text = ctx.resolve(Text(t.text).font(.system(size: size, design: .monospaced)).foregroundColor(ink))
            var c = ctx
            let at = pt(t.position)
            c.translateBy(x: at.x, y: at.y)
            if t.rotation != 0 { c.rotate(by: .radians(-t.rotation)) }
            c.draw(text, at: .zero, anchor: anchor(t.alignment))
        }
    }

    private func anchor(_ a: String) -> UnitPoint {
        switch a {
        case "TopLeft": .topLeading
        case "TopCenter": .top
        case "TopRight": .topTrailing
        case "MiddleLeft": .leading
        case "MiddleRight": .trailing
        case "BottomLeft": .bottomLeading
        case "BottomCenter": .bottom
        case "BottomRight": .bottomTrailing
        default: .center
        }
    }
}
#endif
