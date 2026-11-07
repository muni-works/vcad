import SwiftUI
import CVcadFFI

// 3D print (Fabricate ▸ 3D Print): web PrintPanel / SlicerSettings parity
// over vcad_print_call — printer + material, slicer settings, printability
// analysis with recommended settings, slicing with print-time / filament
// prediction and a layer preview, and G-code / Bambu .gcode.3mf / 3MF export.
// STL stays in File ▸ Export.

@MainActor @Observable
final class PrintStore {
    var printers: [(id: String, name: String)] = []
    var printer = "bambu_p1s"
    var material = "PLA"
    var layerHeight = 0.2
    var walls = 3
    var infillPct = 15.0
    var pattern = "grid"
    var supports = false
    var printTemp = 210
    var bedTemp = 60

    var busy = false
    var error: String?
    var analysis: [String: Any]?
    var slice: [String: Any]?
    var layer = 0
    var lastExport: String?

    func loadPrinters() {
        guard printers.isEmpty, let r = try? FabFFI.call2(vcad_print_call, doc: nil, ["op": "profiles"]),
              let list = r["profiles"] as? [[String: Any]] else { return }
        printers = list.compactMap { p in
            guard let id = p["id"] as? String else { return nil }
            return (id, p["name"] as? String ?? id)
        }
        if !printers.contains(where: { $0.id == printer }), let first = printers.first { printer = first.id }
    }

    var settings: [String: Any] {
        ["printer": printer, "material": material, "layer_height": layerHeight, "wall_count": walls,
         "infill_density": infillPct / 100, "infill_pattern": pattern, "support_enabled": supports,
         "print_temp": printTemp, "bed_temp": bedTemp,
         "first_layer_height": max(layerHeight, 0.2)]
    }

    private func run(_ model: EditorModel, _ request: [String: Any]) async -> [String: Any]? {
        let req = FabFFI.Box(v: request)
        guard let doc = AnalysisStore.documentData(model) else { error = "No document is open."; return nil }
        busy = true; error = nil
        defer { busy = false }
        do { return try await FabFFI.off(box: req) { try FabFFI.call2(vcad_print_call, doc: doc, $0) } }
        catch { self.error = error.localizedDescription; return nil }
    }

    func analyze(_ model: EditorModel) async {
        let part = model.selectedPartIndex ?? 0
        analysis = await run(model, ["op": "analyze", "part": part, "settings": settings])
    }

    func applyRecommended() {
        guard let rec = analysis?["recommended"] as? [String: Any] else { return }
        let s = rec["settings"] as? [String: Any] ?? rec
        if let v = s["layer_height"] as? NSNumber { layerHeight = v.doubleValue }
        if let v = s["wall_count"] as? NSNumber { walls = v.intValue }
        if let v = s["infill_density"] as? NSNumber { infillPct = v.doubleValue * 100 }
        if let v = s["support_enabled"] as? Bool { supports = v }
    }

    func sliceNow(_ model: EditorModel) async {
        slice = await run(model, ["op": "slice", "settings": settings, "maxPreviewLayers": 400])
        layer = 0
    }

    func export(_ model: EditorModel, kind: String) async {
        let name = model.documentName.isEmpty ? "part" : model.documentName
        var req: [String: Any] = ["settings": settings, "name": name]
        switch kind {
        case "gcode":
            guard let url = FabFFI.savePanel("\(name).gcode", ext: "gcode") else { return }
            req["op"] = "slice"; req["gcodePath"] = url.path
        case "bambu":
            guard let url = FabFFI.savePanel("\(name).gcode.3mf", ext: "3mf") else { return }
            req["op"] = "slice"; req["threeMfPath"] = url.path
        default:
            guard let url = FabFFI.savePanel("\(name).3mf", ext: "3mf") else { return }
            req["op"] = "export_3mf"; req["path"] = url.path
        }
        if let r = await run(model, req) {
            lastExport = (r["gcodePath"] ?? r["threeMfPath"] ?? r["path"]) as? String
            if r["stats"] != nil { slice = r }
        }
    }
}

struct PrintWorkbench: View {
    @Bindable var model: EditorModel
    @Bindable var store: PrintStore

    var body: some View {
        Picker("Printer", selection: $store.printer) {
            ForEach(store.printers, id: \.id) { Text($0.name).tag($0.id) }
            if store.printers.isEmpty { Text(store.printer).tag(store.printer) }
        }
        .onAppear { store.loadPrinters() }
        Picker("Material", selection: $store.material) {
            ForEach(["PLA", "PETG", "ABS", "TPU"], id: \.self) { Text($0).tag($0) }
        }
        Eyebrow("Slicer")
        FabField(label: "Layer height", value: $store.layerHeight)
        Stepper("Walls \(store.walls)", value: $store.walls, in: 1...10)
        FabField(label: "Infill %", value: $store.infillPct, digits: 0)
        Picker("Pattern", selection: $store.pattern) {
            ForEach(["grid", "lines", "triangles", "honeycomb", "gyroid"], id: \.self) { Text($0.capitalized).tag($0) }
        }
        Toggle("Supports", isOn: $store.supports)
        HStack {
            Stepper("Nozzle \(store.printTemp)°", value: $store.printTemp, in: 150...320, step: 5).font(.caption)
            Stepper("Bed \(store.bedTemp)°", value: $store.bedTemp, in: 0...120, step: 5).font(.caption)
        }
        HStack {
            Button("Analyze") { Task { await store.analyze(model) } }
            Button { Task { await store.sliceNow(model) } } label: {
                if store.busy { ProgressView().controlSize(.small) } else { Text("Slice") }
            }
            .keyboardShortcut(.return, modifiers: [.command])
        }
        .disabled(store.busy)
        if let e = store.error { Text(e).font(.caption).foregroundStyle(.red).textSelection(.enabled) }
        if let a = store.analysis { analysisView(a) }
        if let s = store.slice { sliceView(s) }
        Eyebrow("Export")
        HStack {
            Button("G-code") { Task { await store.export(model, kind: "gcode") } }
            Button("Bambu 3MF") { Task { await store.export(model, kind: "bambu") } }
                .help("Sliced .gcode.3mf, ready to send to a Bambu printer")
            Button("3MF") { Task { await store.export(model, kind: "3mf") } }
        }
        .disabled(store.busy)
        if let p = store.lastExport {
            Label((p as NSString).lastPathComponent, systemImage: "checkmark.circle").font(.caption).foregroundStyle(.green)
        }
    }

    @ViewBuilder private func analysisView(_ a: [String: Any]) -> some View {
        Eyebrow("Printability")
        if let c = a["cost"] as? [String: Any] {
            Text("≈ \(fabFmt(fabNum(c["weight_grams"]))) g · $\(fabFmt(fabNum(c["total_usd"]), 2)) (pre-slice estimate)")
                .font(.caption.monospacedDigit())
        }
        let warnings = (a["dfm"] as? [String: Any])?["warnings"] as? [[String: Any]] ?? []
        ForEach(Array(warnings.prefix(8).enumerated()), id: \.offset) { _, w in
            Label(w["message"] as? String ?? "\(w)", systemImage: "exclamationmark.triangle")
                .font(.caption).foregroundStyle(.orange)
        }
        if warnings.isEmpty, a["dfm"] != nil {
            Label("No printability issues.", systemImage: "checkmark.seal").font(.caption).foregroundStyle(.green)
        }
        if a["recommended"] != nil { Button("Use recommended settings") { store.applyRecommended() } }
    }

    @ViewBuilder private func sliceView(_ s: [String: Any]) -> some View {
        let stats = s["stats"] as? [String: Any] ?? [:]
        let layers = s["layers"] as? [[String: Any]] ?? []
        Eyebrow("Prediction")
        Text("\(fabDuration(fabNum(stats["print_time_seconds"]))) · \(fabFmt(fabNum(stats["filament_grams"]))) g · \(fabFmt(fabNum(stats["filament_mm"]) / 1000, 2)) m · \(stats["layer_count"] as? Int ?? 0) layers")
            .font(.caption.monospacedDigit())
        if (s["fitsBed"] as? Bool) == false {
            Label("Part is larger than the \(s["printer"] as? String ?? "printer") build volume.", systemImage: "xmark.octagon")
                .font(.caption).foregroundStyle(.red)
        }
        if !layers.isEmpty {
            let i = min(store.layer, layers.count - 1)
            let l = layers[i]
            FabRingsCanvas(closed: fabRings(l["outer"]), open: fabRings(l["infill"]), accent: fabRings(l["inner"]), height: 160)
            Slider(value: Binding(get: { Double(i) }, set: { store.layer = Int($0) }), in: 0...Double(max(1, layers.count - 1)))
            Text("Layer \((l["index"] as? Int ?? 0) + 1) · z \(fabFmt(fabNum(l["z"]), 2)) mm").font(.caption2.monospacedDigit()).foregroundStyle(.secondary)
        }
    }
}
