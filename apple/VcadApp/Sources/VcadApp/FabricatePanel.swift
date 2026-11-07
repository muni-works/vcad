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

@MainActor @Observable
final class FabStore {
    static let shared = FabStore()
    var shown = false
    var tab: FabTab = .sheet
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
    var body: some View {
        LabeledContent(label) {
            TextField("", value: $value, format: .number.precision(.fractionLength(0...digits)))
                .multilineTextAlignment(.trailing).frame(width: 72)
        }
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

struct FabricatePanel: View {
    @Bindable var model: EditorModel
    @Bindable var store = FabStore.shared

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            PanelHeader(title: "Fabricate", systemImage: "hammer", onClose: { store.shown = false })
            Picker("", selection: $store.tab) {
                ForEach(FabTab.allCases) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented).labelsHidden()
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
        .padding(Theme.Space.l).frame(width: 320).panelSurface()
        .accessibilityIdentifier("fabricate-panel")
    }
}

struct FabricateCommands: Commands {
    var body: some Commands {
        CommandMenu("Fabricate") {
            Button("Fabricate Panel") { FabStore.shared.shown.toggle() }
                .keyboardShortcut("f", modifiers: [.command, .option])
            Divider()
            ForEach(FabTab.allCases) { tab in
                Button(tab == .sheet ? "Sheet Metal: Flat Pattern" : tab == .print ? "3D Print: Slice & Export" : "CAM Job: Tools & Verify") {
                    FabStore.shared.shown = true; FabStore.shared.tab = tab
                }
            }
        }
    }
}
