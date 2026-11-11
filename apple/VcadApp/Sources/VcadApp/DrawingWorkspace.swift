#if os(macOS)
import AppKit
import CVcadFFI
import Foundation
import SwiftUI
import UniformTypeIdentifiers

// The Drawing workspace: 2D shop drawings on vcad-kernel-drafting, through
// vcad-ffi's `vcad_drawing_*` (crates/vcad-ffi/src/drafting.rs).
//
// One click from a part or assembly to a full drawing: the default "Sheet"
// view lays out front / top / right / isometric (third-angle), picks the sheet
// size and a standard scale, and fills the title block from the document. The
// kernel composes everything — automatic dimensions (overall size, hole
// callouts with locations, center marks), the user's dimensions and center
// lines, section and detail views, GD&T frames and datum tags, title block,
// BOM — and hands back its display list. This view draws that list as-is, so
// the sheet on screen is the PDF File ▸ Export Drawing writes; DXF is the same
// sheet in model millimetres (views at true size).
//
// Everything the user authors persists on the document (the engine's
// `drawing-settings` feature → IR `drawing`) and every annotation is
// regenerated from the model on each change, so the drawing stays associative.

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
        var name: String?
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

        /// The view's sheet rectangle, grown by `pad` mm.
        func sheetRect(pad: Double = 0) -> (min: DrawingPoint, max: DrawingPoint) {
            let hw = (bounds.max_x - bounds.min_x) / 2 * scale, hh = (bounds.max_y - bounds.min_y) / 2 * scale
            return (DrawingPoint(x: sheet_center[0] - hw - pad, y: sheet_center[1] - hh - pad),
                    DrawingPoint(x: sheet_center[0] + hw + pad, y: sheet_center[1] + hh + pad))
        }
    }
    /// A selectable annotation (`DimensionHandle`).
    struct Handle: Decodable, Equatable {
        let id: String
        let view: String
        let auto: Bool
        let min: [Double]
        let max: [Double]
        let offset: Double
        let kind: String

        func contains(_ p: DrawingPoint, pad: Double = 1.5) -> Bool {
            p.x >= min[0] - pad && p.x <= max[0] + pad && p.y >= min[1] - pad && p.y <= max[1] + pad
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
    var views: [Placement]?
    var dimensions: [Handle]?
    var circles: Int?
    var sheet_size: String?

    /// Placements of every view (the main one alone on older kernels).
    var placements: [Placement] { views ?? [main] }

    /// The view under a sheet point (nearest, within 15 mm of its outline).
    func view(at p: DrawingPoint) -> Placement? {
        func dist(_ v: Placement) -> Double {
            let r = v.sheetRect()
            let dx = Swift.max(r.min.x - p.x, 0, p.x - r.max.x), dy = Swift.max(r.min.y - p.y, 0, p.y - r.max.y)
            return (dx * dx + dy * dy).squareRoot()
        }
        return placements.filter { dist($0) <= 15 }.min { dist($0) < dist($1) }
    }

    /// The topmost annotation under a sheet point (smallest box wins).
    func handle(at p: DrawingPoint) -> Handle? {
        (dimensions ?? []).filter { $0.contains(p) }
            .min { ($0.max[0] - $0.min[0]) * ($0.max[1] - $0.min[1]) < ($1.max[0] - $1.min[0]) * ($1.max[1] - $1.min[1]) }
    }

    /// Nearest drawn vertex (visible/hidden line end, arc center) within
    /// `radius` sheet mm — what dimension clicks snap to.
    func snap(_ p: DrawingPoint, radius: Double = 2.5) -> DrawingPoint {
        var best = p, bestD = radius * radius
        func consider(_ q: DrawingPoint) {
            let d = (q.x - p.x) * (q.x - p.x) + (q.y - p.y) * (q.y - p.y)
            if d < bestD { bestD = d; best = q }
        }
        for l in sheet.lines where l.class == "visible" || l.class == "hidden" { consider(l.start); consider(l.end) }
        for a in sheet.arcs where a.class == "visible" { consider(a.arc.center) }
        // Center-mark crossings are hole centers.
        for l in sheet.lines where l.class == "center" {
            consider(DrawingPoint(x: (l.start.x + l.end.x) / 2, y: (l.start.y + l.end.y) / 2))
        }
        return best
    }
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

/// IR `DrawingDimension`: a user dimension or center line.
struct DrawingDimension: Codable, Equatable, Identifiable {
    var id = "dim-\(UUID().uuidString.prefix(8))"
    var view: String
    /// `horizontal`, `vertical`, `aligned`, or `centerline`.
    var kind: String
    var a: [Double]
    var b: [Double]
    var offset: Double = 0
}

/// The annotation half of IR `DrawingSettings` — persisted as the
/// `drawing-settings` feature's `annotations` JSON.
struct DrawingAnnotationsIR: Codable, Equatable {
    var sheetSize: String?
    var dimensions: [DrawingDimension]?
    var hiddenDimensions: [String]?
    var dimensionOffsets: [String: Double]?
    var details: [DrawingDetail]?
    var gdt: [DrawingGdt]?
    var centerMarks: Bool?
    var holeCallouts: Bool?
}

/// IR `DrawingSettings`.
struct DrawingSettingsIR: Codable, Equatable {
    var titleBlock: DrawingTitleBlock?
    var sections: [DrawingSectionLine]?
    var showBom: Bool?
    var sheetSize: String?
    var dimensions: [DrawingDimension]?
    var hiddenDimensions: [String]?
    var dimensionOffsets: [String: Double]?
    var details: [DrawingDetail]?
    var gdt: [DrawingGdt]?
    var centerMarks: Bool?
    var holeCallouts: Bool?

    var annotations: DrawingAnnotationsIR {
        DrawingAnnotationsIR(sheetSize: sheetSize, dimensions: dimensions, hiddenDimensions: hiddenDimensions,
                             dimensionOffsets: dimensionOffsets, details: details, gdt: gdt,
                             centerMarks: centerMarks, holeCallouts: holeCallouts)
    }
}

/// IR `DrawingDetailSpec`.
struct DrawingDetail: Codable, Equatable, Identifiable {
    var id: String { label }
    var label: String
    var view: String?
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

/// IR `DrawingGdtSpec`.
struct DrawingGdt: Codable, Equatable, Identifiable {
    var id = "gdt-\(UUID().uuidString.prefix(8))"
    var view: String?
    var kind: GdtKind
    var tolerance: Double
    var datum: String?
    var at: [Double]
}

/// Identification letters for section views, detail views and datum
/// features. They share one alphabet so a letter never names two things,
/// and I, O and Q are never used (ASME Y14.3 for views, Y14.5 for datums).
enum DrawingLetters {
    static let alphabet = Array("ABCDEFGHJKLMNPRSTUVWXYZ").map(String.init)

    static func used(sections: [DrawingSectionLine], details: [DrawingDetail], gdt: [DrawingGdt]) -> Set<String> {
        Set(sections.map(\.label) + details.map(\.label) + gdt.compactMap(\.datum))
    }

    /// First allowed letter not in `used`; doubles (AA, BB…) once the
    /// alphabet runs out, as the standards allow.
    static func next(excluding used: Set<String>) -> String {
        for n in 1...3 {
            for c in alphabet where !used.contains(String(repeating: c, count: n)) {
                return String(repeating: c, count: n)
            }
        }
        return "ZZZ"
    }

    /// `wanted` if it is allowed and free, otherwise the next free letter.
    static func resolve(_ wanted: String?, excluding used: Set<String>) -> String {
        if let w = wanted, alphabet.contains(w), !used.contains(w) { return w }
        return next(excluding: used)
    }
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
    /// Front / top / right / isometric on one auto-sized sheet.
    case standard, front, back, top, bottom, left, right, isometric
    var id: String { rawValue }
    var label: String { self == .standard ? "Standard Sheet" : rawValue.capitalized }
    var canSection: Bool { self != .isometric }
}

/// Sheet sizes offered in the inspector (`auto` picks one).
enum DrawingSheetSize: String, CaseIterable, Identifiable {
    case auto, a4, a3, a2, a1, a0, letter
    var id: String { rawValue }
    var label: String { self == .auto ? "Auto" : self == .letter ? "Letter" : rawValue.uppercased() }
}

enum DrawingExportFormat {
    case dxf, pdf
    var label: String { self == .dxf ? "DXF" : "PDF" }
    var fileExtension: String { self == .dxf ? "dxf" : "pdf" }
    var contentType: UTType { self == .dxf ? (UTType(filenameExtension: "dxf") ?? .data) : .pdf }
    /// DXF is the whole sheet in model mm: views at true size.
    var ffiCode: UInt32 { self == .dxf ? UInt32(VCAD_DRAWING_SHEET_DXF) : UInt32(VCAD_DRAWING_PDF) }
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
    case select, dimension, centerLine, detail, section, gdt(GdtKind)
    var hint: String? {
        switch self {
        case .select: nil
        case .dimension: "Click two points to dimension (snaps to corners and hole centers)"
        case .centerLine: "Click the two ends of the center line"
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

    var view: DrawingView = .standard { didSet { if view != oldValue { zoom = 1; pan = .zero; selection = nil; refresh() } } }
    var showHidden = true { didSet { refresh() } }
    var showDimensions = true { didSet { refresh() } }
    /// Detail views (persisted on the document).
    var details: [DrawingDetail] {
        get { documentSettings.details ?? [] }
        set { updateSettings(name: newValue.count > details.count ? "Add Detail" : "Edit Details") { $0.details = newValue } }
    }
    /// GD&T frames and datum tags (persisted on the document).
    var gdt: [DrawingGdt] {
        get { documentSettings.gdt ?? [] }
        set { updateSettings(name: newValue.count > gdt.count ? "Add GD&T" : "Edit GD&T") { $0.gdt = newValue } }
    }
    var tool: DrawingTool = .select { didSet { pendingPoint = nil } }
    /// Selected annotation id (dimension, callout, center line, GD&T).
    var selection: String?
    /// First click of a two-click tool, sheet mm.
    var pendingPoint: (view: String, sheet: DrawingPoint)?
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

    var sectionsForView: [DrawingSectionLine] { (documentSettings.sections ?? []).filter { sectionViews.contains($0.view) } }

    /// Settings as drawn: a sheet with no title block yet gets one filled
    /// from the document (name, today's date), so one click gives a complete
    /// sheet; editing it persists real fields.
    var effectiveSettings: DrawingSettingsIR {
        var s = documentSettings
        if s.titleBlock == nil {
            var tb = DrawingTitleBlock()
            tb.partName = model?.documentName ?? ""
            tb.date = String(ISO8601DateFormatter().string(from: Date()).prefix(10))
            s.titleBlock = tb
        }
        return s
    }

    private func requestData() -> Data {
        let wire = DrawingRequestWire(view: view.rawValue, show_hidden: showHidden, show_dimensions: showDimensions,
                                      details: [], gdt: [], drawing: effectiveSettings,
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
                                                       showBom: str(s.showBom), annotations: str(s.annotations))
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

    /// Letters already naming something on the sheet: sections, details and
    /// datum features (and datums referenced by feature control frames).
    /// Views and datums share one alphabet so "A" is never both.
    var usedLetters: Set<String> {
        DrawingLetters.used(sections: documentSettings.sections ?? [], details: details, gdt: gdt)
    }

    /// Views a section can be cut on in the current layout.
    var sectionViews: Set<String> { view == .standard ? ["front", "top", "right"] : [view.rawValue] }

    func addSection(atViewX x: Double, on placement: DrawingSheetResult.Placement? = nil) {
        let p = placement ?? result?.main
        let name = p?.name ?? view.rawValue
        guard name != "isometric", let b = p?.bounds else { return }
        let label = DrawingLetters.next(excluding: usedLetters)
        let pad = max(b.max_y - b.min_y, 1) * 0.1
        let line = DrawingSectionLine(id: "section-\(UUID().uuidString.prefix(8))", view: name, label: label,
                                      points: [[x, b.min_y - pad], [x, b.max_y + pad]])
        updateSettings(name: "Add Section \(label)-\(label)") { $0.sections = ($0.sections ?? []) + [line] }
    }

    func clearSections() {
        let views = sectionViews
        updateSettings(name: "Clear Sections") { s in s.sections = (s.sections ?? []).filter { !views.contains($0.view) } }
    }

    func addDetail(at p: DrawingPoint, on placement: DrawingSheetResult.Placement? = nil) {
        guard let pl = placement ?? result?.main else { return }
        let b = pl.bounds
        let size = max(min(b.max_x - b.min_x, b.max_y - b.min_y) * 0.3, 1)
        details.append(DrawingDetail(label: DrawingLetters.next(excluding: usedLetters), view: pl.name,
                                     center: [p.x, p.y], width: size, height: size))
    }

    /// Two-click tools: dimension (snapped; horizontal / vertical / aligned
    /// from the pick direction) and center line.
    func addLinear(on view: String, from a: DrawingPoint, to b: DrawingPoint, scale: Double, centerLine: Bool) {
        let dx = b.x - a.x, dy = b.y - a.y
        guard dx * dx + dy * dy > 1e-12 else { return }
        let kind = centerLine ? "centerline" : abs(dy) < abs(dx) * 0.25 ? "horizontal" : abs(dx) < abs(dy) * 0.25 ? "vertical" : "aligned"
        let off = centerLine ? 0 : (kind == "vertical" ? 8.0 : -8.0) / max(scale, 1e-9)
        let dim = DrawingDimension(view: view, kind: kind, a: [a.x, a.y], b: [b.x, b.y], offset: off)
        updateSettings(name: centerLine ? "Add Center Line" : "Add Dimension") { $0.dimensions = ($0.dimensions ?? []) + [dim] }
        selection = dim.id
    }

    /// Delete the selected annotation: a user item is removed, an automatic
    /// one is hidden (Restore Automatic Dimensions brings it back).
    func deleteSelection() {
        guard let id = selection, let h = result?.dimensions?.first(where: { $0.id == id }) else { return }
        delete(h)
    }

    func delete(_ h: DrawingSheetResult.Handle) {
        let id = h.id
        if selection == id { selection = nil }
        if h.kind == "gdt" {
            gdt.removeAll { $0.id == id }
        } else if h.auto {
            updateSettings(name: "Delete Dimension") { $0.hiddenDimensions = ($0.hiddenDimensions ?? []) + [id] }
        } else {
            updateSettings(name: h.kind == "centerline" ? "Delete Center Line" : "Delete Dimension") {
                $0.dimensions = ($0.dimensions ?? []).filter { $0.id != id }
            }
        }
    }

    /// Move a dimension: `delta` sheet mm, converted to its offset (view mm).
    func moveDimension(_ h: DrawingSheetResult.Handle, by delta: DrawingPoint) {
        let s = result?.placements.first { $0.name == h.view }?.scale ?? result?.main.scale ?? 1
        let d: Double
        switch h.kind {
        case "horizontal": d = delta.y / s
        case "vertical": d = delta.x / s
        case "callout": d = (delta.x + delta.y) / 2.squareRoot()
        default: return
        }
        guard abs(d) > 1e-9 else { return }
        let new = h.offset + d
        updateSettings(name: "Move Dimension") { st in
            if h.auto {
                var o = st.dimensionOffsets ?? [:]
                o[h.id] = h.kind == "callout" ? max(new, 1) : new
                st.dimensionOffsets = o
            } else if var dims = st.dimensions, let i = dims.firstIndex(where: { $0.id == h.id }) {
                dims[i].offset = new
                st.dimensions = dims
            }
        }
    }

    func restoreAutomaticDimensions() {
        updateSettings(name: "Restore Automatic Dimensions") { $0.hiddenDimensions = nil; $0.dimensionOffsets = nil }
    }

    func addGdt(_ kind: GdtKind, at p: DrawingPoint, on view: String? = nil) {
        var datum: String? = gdtDatum.isEmpty ? nil : String(gdtDatum.prefix(1)).uppercased()
        if kind == .datum {
            // A new datum feature must not reuse a view letter, another
            // datum's letter, or I/O/Q.
            let taken = DrawingLetters.used(sections: documentSettings.sections ?? [], details: details,
                                            gdt: gdt.filter { $0.kind == .datum })
            datum = DrawingLetters.resolve(datum, excluding: taken)
            gdtDatum = datum ?? gdtDatum
        }
        gdt.append(DrawingGdt(view: view, kind: kind, tolerance: gdtTolerance, datum: datum, at: [p.x, p.y]))
    }

    // MARK: export

    func export(_ format: DrawingExportFormat) {
        guard let model, let doc = model.exportableDocumentData() else {
            presentDrawingAlert("Nothing to export", "Open a .vcad document first.")
            return
        }
        let panel = NSSavePanel()
        panel.allowedContentTypes = [format.contentType]
        panel.nameFieldStringValue = view == .standard
            ? "\(model.documentName).\(format.fileExtension)"
            : "\(model.documentName)-\(view.rawValue).\(format.fileExtension)"
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
                    // Clear of the status field above and the status pill and dock below.
                    DrawingSheetCanvas(result: r, store: store)
                        .padding(.top, Glass.toolbarHeight)
                        .padding(.bottom, Glass.dockClearance + 40)
                } else if let e = store.error {
                    ContentUnavailableView("No Drawing", systemImage: "ruler", description: Text(e))
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    ProgressView("Projecting views…")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
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
                if DrawingStatusPill.hasContent(busy: store.busy, hint: store.tool.hint,
                                                error: store.error, hasResult: store.result != nil) {
                    DrawingStatusPill(store: store).padding(.bottom, Glass.dockClearance)
                }
            }
        }
        .animation(Motion.panel, value: model.showsTree)
        .animation(Motion.panel, value: model.showsInspector)
        .onAppear { store.model = model; store.refresh(force: true) }
        .onChange(of: model.source) { _, _ in store.selection = nil; store.refresh(force: true) }
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
            if let r = store.result, let size = r.sheet_size {
                Text("\(size) · \(r.main.scale_note)").font(.caption).foregroundStyle(.secondary).monospacedDigit()
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
            PanelHeader(title: store.view == .standard ? "Sheet" : "\(store.view.label) View")
            Divider()
            Picker("Size", selection: Binding(get: { DrawingSheetSize(rawValue: store.documentSettings.sheetSize ?? "auto") ?? .auto },
                                              set: { v in store.updateSettings(name: "Sheet Size") { $0.sheetSize = v == .auto ? nil : v.rawValue } })) {
                ForEach(DrawingSheetSize.allCases) { Text($0.label).tag($0) }
            }
            .font(.callout).disabled(store.view != .standard)
            Toggle("Hidden lines", isOn: $store.showHidden).font(.callout)
            Toggle("Dimensions", isOn: $store.showDimensions).font(.callout)
            Toggle("Hole callouts", isOn: Binding(get: { store.documentSettings.holeCallouts ?? true },
                                                  set: { v in store.updateSettings(name: v ? "Show Hole Callouts" : "Hide Hole Callouts") { $0.holeCallouts = v ? nil : false } }))
                .font(.callout)
            Toggle("Center marks", isOn: Binding(get: { store.documentSettings.centerMarks ?? true },
                                                 set: { v in store.updateSettings(name: v ? "Show Center Marks" : "Hide Center Marks") { $0.centerMarks = v ? nil : false } }))
                .font(.callout)
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
            if let id = store.selection, let h = store.result?.dimensions?.first(where: { $0.id == id }) {
                Divider()
                Eyebrow(h.auto ? "Automatic \(h.kind)" : h.kind.capitalized)
                Text("Drag to move · ⌫ to delete").font(.caption).foregroundStyle(.secondary)
                Button(h.auto ? "Hide" : "Delete", role: .destructive) { store.deleteSelection() }
            }
            Menu("Clear") {
                Button("Restore Automatic Dimensions") { store.restoreAutomaticDimensions() }
                    .disabled((store.documentSettings.hiddenDimensions ?? []).isEmpty && (store.documentSettings.dimensionOffsets ?? [:]).isEmpty)
                Button("Clear My Dimensions") { store.updateSettings(name: "Clear Dimensions") { $0.dimensions = nil } }
                    .disabled((store.documentSettings.dimensions ?? []).isEmpty)
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
struct DrawingStatusPill: View {
    let store: DrawingStore
    /// The pill shows only when it has something to say — not over the
    /// "No Drawing" empty state as an empty capsule.
    static func hasContent(busy: Bool, hint: String?, error: String?, hasResult: Bool) -> Bool {
        busy || hint != nil || hasResult
    }
    var body: some View {
        HStack(spacing: 8) {
            if store.busy { ProgressView().controlSize(.mini) }
            if let hint = store.tool.hint {
                Label(hint, systemImage: "hand.point.up.left")
            } else if let e = store.error, store.result != nil {
                Label(e, systemImage: "exclamationmark.triangle")
            } else if let r = store.result {
                Text("\(r.sheet_size ?? "A4") · \(r.main.scale_note) · \(r.placements.count) view\(r.placements.count == 1 ? "" : "s") · \(r.dimensions?.count ?? 0) annotations · \(r.circles ?? 0) holes")
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
    /// Annotation being dragged and the sheet-mm offset so far.
    @State private var moving: (handle: DrawingSheetResult.Handle, delta: DrawingPoint)?
    @State private var hover: DrawingPoint?
    @FocusState private var focused: Bool

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
                overlay(ctx: &ctx, fit: f.fit, origin: f.origin)
            }
            .contentShape(Rectangle())
            .focusable()
            .focusEffectDisabled()
            .focused($focused)
            .onDeleteCommand { store.deleteSelection() }
            .onKeyPress(.escape) { store.selection = nil; store.tool = .select; return .handled }
            .onContinuousHover { phase in
                if case let .active(p) = phase { hover = toSheet(p, fit: f.fit, origin: f.origin) } else { hover = nil }
            }
            .gesture(DragGesture(minimumDistance: 4)
                .onChanged { v in
                    // Dragging an annotation moves it; dragging paper pans.
                    if moving == nil, dragStart == nil, store.tool == .select,
                       let h = result.handle(at: toSheet(v.startLocation, fit: f.fit, origin: f.origin)),
                       ["horizontal", "vertical", "callout"].contains(h.kind) {
                        store.selection = h.id
                        moving = (h, DrawingPoint(x: 0, y: 0))
                    }
                    if let m = moving {
                        moving = (m.handle, DrawingPoint(x: Double(v.translation.width / f.fit),
                                                         y: -Double(v.translation.height / f.fit)))
                        return
                    }
                    if dragStart == nil { dragStart = store.pan }
                    store.pan = CGSize(width: dragStart!.width + v.translation.width,
                                       height: dragStart!.height + v.translation.height)
                }
                .onEnded { _ in
                    if let m = moving { store.moveDimension(m.handle, by: m.delta) }
                    moving = nil
                    dragStart = nil
                })
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

    private func toSheet(_ p: CGPoint, fit: CGFloat, origin: CGPoint) -> DrawingPoint {
        DrawingPoint(x: Double((p.x - origin.x) / fit), y: result.height - Double((p.y - origin.y) / fit))
    }

    private func click(_ p: CGPoint, fit: CGFloat, origin: CGPoint) {
        focused = true
        let sheet = toSheet(p, fit: fit, origin: origin)
        if store.tool == .select {
            store.selection = result.handle(at: sheet)?.id
            return
        }
        guard let placement = result.view(at: sheet) ?? (result.placements.count == 1 ? result.main : nil) else { return }
        let name = placement.name ?? store.view.rawValue
        switch store.tool {
        case .select: return
        case .dimension, .centerLine:
            guard name != "isometric" else { return }
            let snapped = store.tool == .dimension ? result.snap(sheet) : sheet
            if let first = store.pendingPoint, first.view == name {
                store.addLinear(on: name, from: placement.toView(first.sheet), to: placement.toView(snapped),
                                scale: placement.scale, centerLine: store.tool == .centerLine)
                store.pendingPoint = nil
            } else {
                store.pendingPoint = (name, snapped)
            }
            return // stay armed for the next one
        case .detail: store.addDetail(at: placement.toView(sheet), on: placement)
        case .section: store.addSection(atViewX: placement.toView(sheet).x, on: placement)
        case let .gdt(kind): store.addGdt(kind, at: placement.toView(sheet), on: placement.name)
        }
        store.tool = .select
    }

    /// Selection box, drag ghost, and the pending first click.
    private func overlay(ctx: inout GraphicsContext, fit: CGFloat, origin: CGPoint) {
        let H = result.height
        func pt(_ p: DrawingPoint) -> CGPoint { CGPoint(x: origin.x + p.x * fit, y: origin.y + (H - p.y) * fit) }
        func box(_ h: DrawingSheetResult.Handle, _ d: DrawingPoint) -> CGRect {
            let a = pt(DrawingPoint(x: h.min[0] - 1 + d.x, y: h.max[1] + 1 + d.y))
            let b = pt(DrawingPoint(x: h.max[0] + 1 + d.x, y: h.min[1] - 1 + d.y))
            return CGRect(x: a.x, y: a.y, width: b.x - a.x, height: b.y - a.y)
        }
        if let id = store.selection, let h = result.dimensions?.first(where: { $0.id == id }) {
            let r = Path(roundedRect: box(h, DrawingPoint(x: 0, y: 0)), cornerRadius: 3)
            ctx.fill(r, with: .color(.accentColor.opacity(0.10)))
            ctx.stroke(r, with: .color(.accentColor), lineWidth: 1)
        }
        if let m = moving {
            ctx.stroke(Path(roundedRect: box(m.handle, m.delta), cornerRadius: 3), with: .color(.accentColor),
                       style: StrokeStyle(lineWidth: 1, dash: [4, 3]))
        }
        if store.tool == .dimension, let h = hover {
            let s = result.snap(h)
            if s != h { ctx.stroke(Path(ellipseIn: CGRect(x: pt(s).x - 4, y: pt(s).y - 4, width: 8, height: 8)), with: .color(.accentColor), lineWidth: 1.5) }
        }
        if let first = store.pendingPoint {
            let a = pt(first.sheet)
            ctx.fill(Path(ellipseIn: CGRect(x: a.x - 3, y: a.y - 3, width: 6, height: 6)), with: .color(.accentColor))
            if let h = hover {
                var p = Path(); p.move(to: a); p.addLine(to: pt(store.tool == .dimension ? result.snap(h) : h))
                ctx.stroke(p, with: .color(.accentColor), style: StrokeStyle(lineWidth: 1, dash: [3, 3]))
            }
        }
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
