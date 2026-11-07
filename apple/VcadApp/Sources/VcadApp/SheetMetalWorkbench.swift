import SwiftUI
import CVcadFFI

// Sheet metal (Fabricate ▸ Sheet Metal): web SheetMetalView parity.
//  • From part — flatten any constant-thickness solid in the document
//    (vcad_sheet_metal_flatten); the DXF is only offered when the recovered
//    flat pattern's volume matches the kernel's BRep volume.
//  • Author — base flange + edge flanges as a chain (vcad_sheet_metal_call),
//    with bend table, manufacturability, cost, nesting, folded STEP, and
//    insertion into the document as SheetMetal IR nodes.

enum SheetMode: String, CaseIterable, Identifiable {
    case part = "From part", author = "Author"
    var id: String { rawValue }
}

struct SheetFlange: Identifiable, Equatable {
    let id = UUID()
    var panel = 0
    var edge = 0
    var length = 25.0
    var angleDeg = 90.0
    var radius = 1.0
    var up = true
}

@MainActor @Observable
final class SheetMetalStore {
    var mode: SheetMode = .part
    var part = 0
    var material = "al-soft"
    var shop = ""
    var quantity = 10
    // Authoring.
    var width = 100.0
    var depth = 60.0
    var thickness = 1.5
    var flanges: [SheetFlange] = [SheetFlange(edge: 0), SheetFlange(edge: 2)]
    // Nesting.
    var sheetWidth = 1219.2
    var sheetHeight = 2438.4
    var nestQty = 10
    // Results.
    var busy = false
    var error: String?
    var result: [String: Any]?
    var cost: [String: Any]?
    var nest: [String: Any]?
    var materials: [(key: String, label: String)] = []

    func loadMaterials() {
        guard materials.isEmpty, let r = try? FabFFI.call1(vcad_sheet_metal_call, ["op": "materials"]),
              let items = r["items"] as? [[String: Any]] else { return }
        materials = items.compactMap { m in
            guard let k = m["name"] as? String else { return nil }
            return (k, (m["display_name"] as? String) ?? k)
        }
    }

    var chain: [[String: Any]] {
        var base: [String: Any] = ["type": "BaseFlangeRect", "width": width, "depth": depth,
                                   "thickness": thickness, "material": material]
        if !shop.isEmpty { base["shopProfile"] = shop }
        return [base] + flanges.map { f in
            ["type": "EdgeFlange", "panelId": f.panel, "edgeIndex": f.edge, "length": f.length,
             "angle": f.angleDeg * .pi / 180, "radius": f.radius, "direction": f.up ? "Up" : "Down"]
        }
    }

    func run(_ model: EditorModel) async {
        busy = true; error = nil; cost = nil; nest = nil
        defer { busy = false }
        do {
            if mode == .part {
                guard let doc = AnalysisStore.documentData(model) else { error = "No document is open."; return }
                var req: [String: Any] = ["part": part, "material": material, "quantity": quantity]
                if !shop.isEmpty { req["shopProfile"] = shop }
                let r = try await FabFFI.off(req) { try FabFFI.call2(vcad_sheet_metal_flatten, doc: doc, $0) }
                result = r
                cost = r["cost"] as? [String: Any]
            } else {
                let c = chain, q = quantity
                let r = try await FabFFI.off(["op": "evaluate", "chain": c]) { try FabFFI.call1(vcad_sheet_metal_call, $0) }
                result = r
                if (r["error"] as? String) == nil {
                    let cr = try await FabFFI.off(["op": "cost", "chain": c, "quantity": q]) {
                        try FabFFI.call1(vcad_sheet_metal_call, $0)
                    }
                    cost = cr["breakdown"] as? [String: Any]
                }
            }
            if let e = result?["error"] as? String { error = e }
        } catch { self.error = error.localizedDescription }
    }

    /// Flat-pattern bounding box (mm), for nesting footprints.
    var flatSize: (Double, Double)? {
        let rings = fabRings((result?["flat_pattern"] as? [String: Any])?["silhouette_2d"])
        let pts = rings.flatMap { $0 }
        guard let x0 = pts.map({ $0[0] }).min(), let x1 = pts.map({ $0[0] }).max(),
              let y0 = pts.map({ $0[1] }).min(), let y1 = pts.map({ $0[1] }).max() else { return nil }
        return (x1 - x0, y1 - y0)
    }

    func runNest() {
        guard let (w, h) = flatSize else { return }
        let parts: [[String: Any]] = [["name": "part", "width_mm": w, "height_mm": h, "quantity": nestQty]]
        let params: [String: Any] = ["stock_width_mm": sheetWidth, "stock_height_mm": sheetHeight,
                                     "spacing_mm": 3.0, "edge_margin_mm": 10.0, "allow_rotation": true]
        do {
            let r = try FabFFI.call1(vcad_sheet_metal_call, ["op": "nest", "parts": parts, "params": params])
            if let e = r["error"] as? String { error = e; return }
            nest = r["result"] as? [String: Any]
        } catch { self.error = error.localizedDescription }
    }

    /// One DXF per sheet (authored chains only — the nested writer needs the chain).
    func exportNested() {
        guard mode == .author, let placements = nest?["placements"] as? [[String: Any]] else { return }
        let c = chain
        let specs: [[String: Any]] = placements.map {
            ["chain": c, "sheet": $0["sheet"] ?? 0, "dxMm": $0["x_mm"] ?? 0, "dyMm": $0["y_mm"] ?? 0,
             "rotated": $0["rotated"] ?? false]
        }
        do {
            let r = try FabFFI.call1(vcad_sheet_metal_call, ["op": "nested_dxf", "placements": specs])
            if let e = r["error"] as? String { error = e; return }
            let sheets = r["sheets"] as? [String] ?? []
            for (i, dxf) in sheets.enumerated() { try FabFFI.save(dxf, name: "nest-sheet-\(i + 1).dxf", ext: "dxf") }
        } catch { self.error = error.localizedDescription }
    }

    func exportStep() {
        do {
            let r = try FabFFI.call1(vcad_sheet_metal_call, ["op": "folded_step", "chain": chain])
            if let e = r["error"] as? String { error = e; return }
            try FabFFI.save(r["step"] as? String ?? "", name: "sheet-metal.step", ext: "step")
        } catch { self.error = error.localizedDescription }
    }

    /// Append the authored chain to the document as SheetMetal IR nodes.
    func insert(_ model: EditorModel) {
        let ok = model.editIR("Sheet Metal Part") { doc in
            var nodes = doc["nodes"] as? [String: Any] ?? [:]
            var next = (nodes.keys.compactMap { Int($0) }.max() ?? 0) + 1
            var base: [String: Any] = ["type": "SheetMetalBaseFlangeRect", "width": width, "depth": depth,
                                       "thickness": thickness, "material": material]
            if !shop.isEmpty { base["shop_profile"] = shop }
            var prev = next
            nodes["\(next)"] = ["id": next, "name": "Base Flange", "op": base]
            for f in flanges {
                next += 1
                nodes["\(next)"] = ["id": next, "name": "Edge Flange", "op": [
                    "type": "SheetMetalEdgeFlange", "parent": prev, "panel_id": f.panel, "edge_index": f.edge,
                    "length": f.length, "angle": f.angleDeg * .pi / 180, "radius": f.radius,
                    "direction": f.up ? "Up" : "Down"] as [String: Any]]
                prev = next
            }
            doc["nodes"] = nodes
            var roots = doc["roots"] as? [[String: Any]] ?? []
            roots.append(["root": prev, "material": "default"])
            doc["roots"] = roots
        }
        if !ok { error = model.engineError ?? "Couldn't add the sheet-metal part." }
    }
}

struct SheetMetalWorkbench: View {
    @Bindable var model: EditorModel
    @Bindable var store: SheetMetalStore

    var body: some View {
        Picker("", selection: $store.mode) {
            ForEach(SheetMode.allCases) { Text($0.rawValue).tag($0) }
        }
        .pickerStyle(.segmented).labelsHidden()
        .onAppear { store.loadMaterials() }

        Picker("Material", selection: $store.material) {
            ForEach(store.materials, id: \.key) { Text($0.label).tag($0.key) }
            if store.materials.isEmpty { Text(store.material).tag(store.material) }
        }
        Picker("Shop", selection: $store.shop) {
            Text("Generic").tag("")
            Text("SendCutSend").tag("sendcutsend")
        }

        if store.mode == .part {
            Stepper("Part \(store.part + 1)", value: $store.part, in: 0...max(0, model.partCount - 1))
            Text("Flattens a constant-thickness solid (extrusion, boolean, STEP) and checks it against the kernel volume.")
                .font(.caption).foregroundStyle(.secondary)
        } else {
            authorForm
        }
        Stepper("Quantity \(store.quantity)", value: $store.quantity, in: 1...10000)

        HStack {
            Button { Task { await store.run(model) } } label: {
                if store.busy { ProgressView().controlSize(.small) } else { Text(store.mode == .part ? "Flatten" : "Unfold") }
            }
            .keyboardShortcut(.return, modifiers: [.command])
            .disabled(store.busy)
            if store.mode == .author {
                Button("Insert") { store.insert(model) }.help("Add this part to the document")
            }
        }
        if let e = store.error { Text(e).font(.caption).foregroundStyle(.red).textSelection(.enabled) }
        if let r = store.result, (r["error"] as? String) == nil { results(r) }
    }

    private var authorForm: some View {
        VStack(alignment: .leading, spacing: Theme.Space.xs) {
            Eyebrow("Base flange")
            FabField(label: "Width", value: $store.width)
            FabField(label: "Depth", value: $store.depth)
            FabField(label: "Thickness", value: $store.thickness, digits: 3)
            HStack {
                Eyebrow("Edge flanges")
                Spacer()
                Button { store.flanges.append(SheetFlange(edge: store.flanges.count % 4)) } label: { Image(systemName: "plus") }
                    .buttonStyle(.borderless)
            }
            ForEach($store.flanges) { $f in
                VStack(alignment: .leading, spacing: 2) {
                    HStack {
                        Stepper("Edge \(f.edge)", value: $f.edge, in: 0...3).font(.caption)
                        Toggle("Up", isOn: $f.up).toggleStyle(.checkbox).font(.caption)
                        Button { store.flanges.removeAll { $0.id == f.id } } label: { Image(systemName: "minus.circle") }
                            .buttonStyle(.borderless)
                    }
                    HStack {
                        FabField(label: "L", value: $f.length, digits: 1)
                        FabField(label: "∠", value: $f.angleDeg, digits: 1)
                        FabField(label: "R", value: $f.radius, digits: 2)
                    }.font(.caption)
                }
                .padding(6).background(RoundedRectangle(cornerRadius: 6).fill(.quaternary.opacity(0.25)))
            }
        }
    }

    @ViewBuilder private func results(_ r: [String: Any]) -> some View {
        let flat = r["flat_pattern"] as? [String: Any] ?? [:]
        let creases = (flat["creases"] as? [[String: Any]] ?? []).compactMap { c -> [[Double]]? in
            (c["line"] as? [[NSNumber]])?.map { $0.map(\.doubleValue) }
        }
        let silhouette = fabRings(flat["silhouette_2d"])
        FabRingsCanvas(closed: silhouette.isEmpty ? fabRings(flat["panel_outlines_2d"]) : silhouette,
                       open: fabRings(flat["engravings_2d"]), accent: creases)
        if let (w, h) = store.flatSize {
            Text("Flat \(fabFmt(w)) × \(fabFmt(h)) mm · area \(fabFmt(fabNum(flat["area_mm2"]), 0)) mm²")
                .font(.caption.monospacedDigit())
        }
        if let check = r["kernelVolumeCheck"] as? [String: Any] {
            let pass = (check["pass"] as? Bool) ?? false
            Label("Volume: solid \(fabFmt(fabNum(check["kernelVolumeMm3"]))) vs flat \(fabFmt(fabNum(check["recoveredVolumeMm3"]))) mm³ (\(fabFmt(fabNum(check["errorFrac"]) * 100, 2))%)",
                  systemImage: pass ? "checkmark.seal" : "xmark.octagon")
                .font(.caption).foregroundStyle(pass ? .green : .red)
        }
        bendTable(r)
        let violations = r["violations"] as? [[String: Any]] ?? []
        if !violations.isEmpty {
            Eyebrow("Manufacturability")
            ForEach(Array(violations.enumerated()), id: \.offset) { _, v in
                Label(v["message"] as? String ?? "", systemImage: (v["severity"] as? String) == "Error" ? "xmark.octagon" : "exclamationmark.triangle")
                    .font(.caption).foregroundStyle((v["severity"] as? String) == "Error" ? .red : .orange)
            }
        }
        if let c = store.cost {
            Eyebrow("Cost")
            Text("\(c["currency"] as? String ?? "USD") \(fabFmt(fabNum(c["total_each"]), 2)) each · \(fabFmt(fabNum(c["total_run"]), 2)) for \(store.quantity)")
                .font(.caption.monospacedDigit())
            Text("material \(fabFmt(fabNum(c["material_each"]), 2)) · cut \(fabFmt(fabNum(c["cut_each"]), 2)) · bend \(fabFmt(fabNum(c["bend_each"]), 2)) · \(fabFmt(fabNum(c["mass_kg_each"]) * 1000, 0)) g")
                .font(.caption2.monospacedDigit()).foregroundStyle(.secondary)
        }
        nesting
        HStack {
            Button("Export DXF") {
                do { try FabFFI.save(r["dxf"] as? String ?? "", name: "flat-pattern.dxf", ext: "dxf") }
                catch { store.error = error.localizedDescription }
            }
            .disabled((r["dxf"] as? String ?? "").isEmpty)
            if store.mode == .author { Button("Folded STEP") { store.exportStep() } }
        }
    }

    @ViewBuilder private func bendTable(_ r: [String: Any]) -> some View {
        let bends = (r["model"] as? [String: Any])?["bends"] as? [[String: Any]] ?? []
        if !bends.isEmpty {
            Eyebrow("Bend table")
            Grid(alignment: .trailing, horizontalSpacing: 8, verticalSpacing: 2) {
                GridRow { Text("#"); Text("Angle"); Text("R"); Text("K"); Text("BA"); Text("Form") }
                    .font(.caption2).foregroundStyle(.secondary)
                ForEach(Array(bends.enumerated()), id: \.offset) { i, b in
                    GridRow {
                        Text("\(i + 1)")
                        Text("\(fabFmt(fabNum(b["angle_rad"]) * 180 / .pi))°")
                        Text(fabFmt(fabNum(b["radius"]), 2))
                        Text(fabFmt(fabNum(b["k_factor"]), 3))
                        Text(fabFmt(fabNum(b["allowance_mm"]), 2))
                        Text("\(fabFmt(fabNum(b["compensated_angle_rad"]) * 180 / .pi))°")
                    }
                    .font(.caption.monospacedDigit())
                }
            }
        }
    }

    @ViewBuilder private var nesting: some View {
        Eyebrow("Nest")
        HStack {
            FabField(label: "Sheet W", value: $store.sheetWidth, digits: 1)
            FabField(label: "H", value: $store.sheetHeight, digits: 1)
        }.font(.caption)
        HStack {
            Stepper("× \(store.nestQty)", value: $store.nestQty, in: 1...5000).font(.caption)
            Button("Nest") { store.runNest() }
            if store.mode == .author, store.nest != nil { Button("Sheet DXFs") { store.exportNested() } }
        }
        if let n = store.nest {
            Text("\(n["sheets_used"] as? Int ?? 0) sheet(s) · \(fabFmt(fabNum(n["utilization_pct"])))% used · \((n["placements"] as? [Any])?.count ?? 0) placed")
                .font(.caption.monospacedDigit())
        }
    }
}
