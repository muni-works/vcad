import SwiftUI
import AppKit
import UniformTypeIdentifiers
import CVcadFFI

// The Fabricate panel (right rail, ⌥⌘F): sheet metal, 3D print and the
// multi-tool CAM job — native parity with the web app's SheetMetalView,
// PrintPanel/SlicerSettings and ToolLibrary/OperationList/CamPanel. Each tab
// lives in its own file (SheetMetalWorkbench, PrintWorkbench, CamJobPanel);
// this file hosts them and the shared FFI plumbing.

enum FabTab: String, CaseIterable, Identifiable {
    case sheet = "Sheet Metal", print = "3D Print", cam = "CAM Job"
    var id: String { rawValue }
}

/// One fabrication run: a part through a process.
struct FabJob: Identifiable, Equatable {
    let id = UUID()
    let process: FabTab
    let part: Int
    let partName: String
    var summary: String
    var updated = Date()
    var symbol: String {
        switch process {
        case .sheet: "square.3.layers.3d.down.right"
        case .print: "printer"
        case .cam: "gearshape.2"
        }
    }
}

@MainActor @Observable
final class FabStore {
    static let shared = FabStore()
    var shown = false
    var tab: FabTab = .sheet
    /// Every fabrication run this session: which part, which process, what
    /// came out. The Fab navigator lists these.
    var jobs: [FabJob] = []
    var selectedJob: FabJob.ID?

    /// Record (or refresh) the job for `part` × `tab` after a run lands.
    func record(_ tab: FabTab, part: Int, partName: String, summary: String) {
        if let i = jobs.firstIndex(where: { $0.process == tab && $0.part == part }) {
            jobs[i].summary = summary; jobs[i].updated = Date()
            selectedJob = jobs[i].id
        } else {
            let j = FabJob(process: tab, part: part, partName: partName, summary: summary)
            jobs.append(j); selectedJob = j.id
        }
    }
    let sheet = SheetMetalStore()
    let print = PrintStore()
    let cam = CamJobStore()
}

/// JSON in / JSON out over the fabrication C ABI. Every entry point returns
/// an owned string (vcad_string_free) or null with vcad_last_error.
enum FabFFI {
    struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    static func take(_ raw: UnsafeMutablePointer<CChar>?) throws -> [String: Any] {
        guard let raw else { throw Failure(message: AnalysisFFI.lastError() ?? "fabrication call failed") }
        defer { vcad_string_free(raw) }
        let data = Data(bytes: raw, count: strlen(raw))
        guard let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            // Some list-returning calls (materials) are arrays; wrap them.
            if let arr = try? JSONSerialization.jsonObject(with: data) as? [Any] { return ["items": arr] }
            throw Failure(message: "unexpected fabrication result")
        }
        return obj
    }

    static func string(_ obj: Any) throws -> String {
        String(decoding: try JSONSerialization.data(withJSONObject: obj, options: [.sortedKeys]), as: UTF8.self)
    }

    /// `char *f(const char *req)`.
    static func call1(_ f: (UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?, _ req: [String: Any]) throws -> [String: Any] {
        let r = try string(req)
        return try take(r.withCString { f($0) })
    }

    /// `char *f(const char *doc, const char *req)`; doc may be nil.
    static func call2(_ f: (UnsafePointer<CChar>?, UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?,
                      doc: Data?, _ req: [String: Any]) throws -> [String: Any] {
        let r = try string(req)
        let d = doc.map { String(decoding: $0, as: UTF8.self) }
        return try take(r.withCString { rp in
            if let d { return d.withCString { dp in f(dp, rp) } }
            return f(nil, rp)
        })
    }

    /// Box so untyped JSON can cross into a detached task and back.
    struct Box: @unchecked Sendable { let v: [String: Any] }

    /// Run a fabrication call off the main actor.
    static func off(_ req: [String: Any],
                    _ body: @escaping @Sendable ([String: Any]) throws -> [String: Any]) async throws -> [String: Any] {
        try await off(box: Box(v: req), body)
    }

    static func off(box: Box,
                    _ body: @escaping @Sendable ([String: Any]) throws -> [String: Any]) async throws -> [String: Any] {
        try await Task.detached { Box(v: try body(box.v)) }.value.v
    }

    /// Ask for a save location; nil when cancelled.
    @MainActor static func savePanel(_ name: String, ext: String) -> URL? {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = name
        panel.allowedContentTypes = [UTType(filenameExtension: ext) ?? .data]
        return panel.runModal() == .OK ? panel.url : nil
    }

    @MainActor static func save(_ text: String, name: String, ext: String) throws {
        guard let url = savePanel(name, ext: ext) else { return }
        try text.write(to: url, atomically: true, encoding: .utf8)
    }
}

func fabNum(_ v: Any?) -> Double { (v as? NSNumber)?.doubleValue ?? 0 }
func fabFmt(_ v: Double, _ digits: Int = 1) -> String { String(format: "%.\(digits)f", v) }
func fabDuration(_ seconds: Double) -> String {
    guard seconds.isFinite, seconds >= 0 else { return "—" }
    let s = Int(seconds.rounded())
    return s >= 3600 ? String(format: "%dh %02dm", s / 3600, (s % 3600) / 60) : String(format: "%dm %02ds", s / 60, s % 60)
}

/// A labelled numeric field, compact enough for the 320 pt rail.
struct FabField: View {
    let label: String
    @Binding var value: Double
    var digits = 2
    // A text draft rather than `TextField(value:format:)`: the formatted
    // field only committed on focus loss (Tab), so Return — and a Generate
    // clicked straight after typing — ran with the old value. Every parseable
    // keystroke now commits, and Return / focus loss normalise the text.
    @State private var draft = ""
    @FocusState private var focused: Bool

    var body: some View {
        // The label keeps its size and the field flexes: three fixed 72 pt
        // fields in a row squeezed the labels to nothing (CAM showed bare numbers).
        HStack(spacing: 3) {
            Text(label).foregroundStyle(.secondary).fixedSize()
            TextField(label, text: $draft)
                .labelsHidden()
                .focused($focused)
                .multilineTextAlignment(.trailing).frame(minWidth: 36, maxWidth: 72)
                .onAppear { draft = Self.format(value, digits) }
                .onChange(of: draft) { _, t in
                    if let v = Self.parse(t), v != value { value = v }
                }
                .onChange(of: value) { _, v in
                    if Self.parse(draft) != v { draft = Self.format(v, digits) }
                }
                .onSubmit { draft = Self.format(value, digits) }
                .onChange(of: focused) { _, f in if !f { draft = Self.format(value, digits) } }
        }
    }

    /// Parses a typed number (locale decimal separator or "."); nil while incomplete.
    static func parse(_ text: String) -> Double? {
        let t = text.trimmingCharacters(in: .whitespaces)
        guard !t.isEmpty else { return nil }
        if let v = Double(t), v.isFinite { return v }
        let sep = Locale.current.decimalSeparator ?? "."
        if sep != ".", let v = Double(t.replacingOccurrences(of: sep, with: ".")), v.isFinite { return v }
        return nil
    }

    static func format(_ v: Double, _ digits: Int) -> String {
        v.formatted(.number.precision(.fractionLength(0...digits)).grouping(.never))
    }
}

/// Draws 2D rings (closed) and polylines fitted to the canvas.
struct FabRingsCanvas: View {
    var closed: [[[Double]]]
    var open: [[[Double]]] = []
    var accent: [[[Double]]] = []
    var height: CGFloat = 180

    var body: some View {
        Canvas { ctx, size in
            let all = (closed + open + accent).flatMap { $0 }
            guard let minX = all.map({ $0[0] }).min(), let maxX = all.map({ $0[0] }).max(),
                  let minY = all.map({ $0[1] }).min(), let maxY = all.map({ $0[1] }).max() else { return }
            let w = max(maxX - minX, 1e-6), h = max(maxY - minY, 1e-6)
            let s = min((size.width - 12) / w, (size.height - 12) / h)
            let ox = (size.width - w * s) / 2, oy = (size.height - h * s) / 2
            func pt(_ p: [Double]) -> CGPoint { CGPoint(x: ox + (p[0] - minX) * s, y: size.height - (oy + (p[1] - minY) * s)) }
            func path(_ ring: [[Double]], close: Bool) -> Path {
                var p = Path()
                guard let f = ring.first else { return p }
                p.move(to: pt(f)); ring.dropFirst().forEach { p.addLine(to: pt($0)) }
                if close { p.closeSubpath() }
                return p
            }
            var fill = Path()
            closed.forEach { fill.addPath(path($0, close: true)) }
            ctx.fill(fill, with: .color(.accentColor.opacity(0.12)), style: FillStyle(eoFill: true))
            ctx.stroke(fill, with: .color(.primary.opacity(0.8)), lineWidth: 1)
            for l in open { ctx.stroke(path(l, close: false), with: .color(.secondary.opacity(0.6)), lineWidth: 0.6) }
            for l in accent {
                ctx.stroke(path(l, close: false), with: .color(.orange), style: StrokeStyle(lineWidth: 1, dash: [4, 3]))
            }
        }
        .frame(height: height)
        .background(RoundedRectangle(cornerRadius: 6).fill(.quaternary.opacity(0.3)))
    }
}

func fabRings(_ v: Any?) -> [[[Double]]] {
    (v as? [[[NSNumber]]])?.map { $0.map { $0.map(\.doubleValue) } } ?? []
}

/// The Fab inspector: the active process's settings and results. The dock
/// picks the process, so there is no tab row here.
struct FabricatePanel: View {
    @Bindable var model: EditorModel
    @Bindable var store = FabStore.shared

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            PanelHeader(title: store.tab.rawValue)
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.Space.m) {
                    switch store.tab {
                    case .sheet: SheetMetalWorkbench(model: model, store: store.sheet)
                    case .print: PrintWorkbench(model: model, store: store.print)
                    case .cam: CamJobPanel(model: model, store: store.cam)
                    }
                }
            }
            .frame(maxHeight: 620).fixedSize(horizontal: false, vertical: true)
        }
        .controlSize(.small)
        .padding(Theme.Space.m).frame(width: Theme.Width.toolInspector).panelSurface()
        .accessibilityIdentifier("fabricate-panel")
        // A run that lands becomes a job in the navigator.
        .onChange(of: store.sheet.busy) { was, now in
            guard was, !now, store.sheet.result != nil else { return }
            record(.sheet, part: store.sheet.part, summary: "Flat pattern · ×\(store.sheet.quantity)")
        }
        .onChange(of: store.print.busy) { was, now in
            guard was, !now, store.print.slice != nil || store.print.analysis != nil else { return }
            record(.print, part: model.selectedPartIndex ?? 0,
                   summary: store.print.slice != nil ? "Sliced · \(store.print.material)" : "Analyzed · \(store.print.material)")
        }
        .onChange(of: store.cam.busy) { was, now in
            guard was, !now, store.cam.result != nil else { return }
            record(.cam, part: model.selectedPartIndex ?? 0, summary: "\(store.cam.ops.count) ops · \(store.cam.post)")
        }
    }

    private func record(_ tab: FabTab, part: Int, summary: String) {
        let name = model.featureNodes.first { $0.partIndex == part }?.name ?? "Part \(part + 1)"
        store.record(tab, part: part, partName: name, summary: summary)
    }
}

/// The Fab navigator: jobs this session, then the checks with their counts.
struct FabJobsNavigator: View {
    @Bindable var model: EditorModel
    @Bindable var store = FabStore.shared
    @Bindable var analysis = AnalysisStore.shared

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            PanelHeader(title: "Jobs")
            Divider()
            if store.jobs.isEmpty {
                Text("Run Flatten, Slice, or Post to add a job.").font(.caption).foregroundStyle(.secondary)
            }
            ForEach(store.jobs) { j in
                Button {
                    store.selectedJob = j.id; store.tab = j.process
                    if let id = model.featureNodes.first(where: { $0.partIndex == j.part })?.id { model.selectFeature(id) }
                } label: {
                    HStack(alignment: .top, spacing: 6) {
                        Image(systemName: j.symbol).frame(width: 16)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(j.partName).lineLimit(1)
                            Text(j.summary).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                        }
                        Spacer(minLength: 0)
                    }
                    .font(.callout).padding(.horizontal, 6).padding(.vertical, 3)
                    .contentShape(Rectangle())
                    .selectableRow(selected: store.selectedJob == j.id)
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(store.selectedJob == j.id ? .isSelected : [])
            }
            Divider()
            Eyebrow("Checks")
            checkRow("DFM", "checkmark.shield", analysis.dfmReport.map { "\($0.issues.count) issue\($0.issues.count == 1 ? "" : "s")" }) {
                analysis.shown = true; analysis.tab = .dfm
            }
            checkRow("Stress", "waveform.path.ecg", analysis.feaResult.map { r in
                r.safety_factor.map { String(format: "SF %.1f", $0) } ?? String(format: "%.0f MPa", r.max_von_mises_mpa)
            }) { analysis.shown = true; analysis.tab = .fea }
        }
        .padding(Theme.Space.m).frame(width: Theme.Width.navigator).panelSurface()
    }

    private func checkRow(_ title: String, _ symbol: String, _ value: String?, _ open: @escaping () -> Void) -> some View {
        Button(action: open) {
            HStack {
                Label(title, systemImage: symbol)
                Spacer(minLength: 0)
                Text(value ?? "Not run").foregroundStyle(.secondary).monospacedDigit()
            }.font(.callout).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("Open \(title)")
    }
}

struct FabricateCommands: Commands {
    var body: some Commands {
        CommandMenu("Fabricate") {
            Button("Fabricate Panel") { FabStore.shared.shown.toggle() }
                .keyboardShortcut("f", modifiers: [.command, .option])
            Divider()
            Button("Tune Instrument (Microphone)") { TuneStore.shared.shown.toggle() }
                .keyboardShortcut("t", modifiers: [.command, .option])
            Divider()
            ForEach(FabTab.allCases) { tab in
                Button(tab == .sheet ? "Sheet Metal: Flat Pattern" : tab == .print ? "3D Print: Slice & Export" : "CAM Job: Tools & Verify") {
                    FabStore.shared.shown = true; FabStore.shared.tab = tab
                }
            }
        }
    }
}
