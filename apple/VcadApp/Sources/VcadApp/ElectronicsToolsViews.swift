import SwiftUI
import AppKit

/// Command-bar menus for the kernel-backed electronics tools.
struct ElectronicsToolMenus: View {
    @Bindable var model: EditorModel
    private var ec: ElectronicsWorkspace { model.electronics }
    var body: some View {
        let t = ec.tools, hasBoard = !ec.board(model).isEmpty
        HStack(spacing: 8) {
            if let busy = t.busy { ProgressView().controlSize(.small); Text("\(busy)…").foregroundStyle(.secondary) }
            Menu("Board") {
                Button("Import KiCad / Eagle Board…") { t.importBoard(ec, model) }
                Divider()
                Button("Autoroute") { t.autoroute(ec, model) }.disabled(!hasBoard)
                Button("Run DRC") { t.runDrc(ec, model) }.disabled(!hasBoard)
                Button("Fix DRC") { t.fixDrc(ec, model) }.disabled(!hasBoard)
                Button("Fix DRC with Rule Calibration") { t.fixDrc(ec, model, calibrate: true) }.disabled(!hasBoard)
                Divider()
                Button("Length Tuning…") { t.openLengths(ec, model) }.disabled(!hasBoard)
                Button("Layer Stack…") { t.sheet = .stackup }.disabled(!hasBoard)
                Divider()
                Button("Export Gerbers…") { t.exportGerbers(ec, model) }.disabled(!hasBoard)
            }.fixedSize()
            Menu("Enclosure") {
                Button("Enclose Board") { t.encloseBoard(ec, model) }.disabled(!hasBoard)
                Button("Fit Check…") { t.sheet = .enclosure }.disabled(!hasBoard)
                Divider()
                Button("Board from Enclosure Cavity") { t.boardFromSolid(ec, model) }
                Button("Add Board as Solid Part") { t.solidFromBoard(ec, model) }.disabled(!hasBoard)
            }.fixedSize()
            Button("Simulate…") { t.sheet = .simulate }
        }
        .disabled(t.busy != nil)
    }
}

/// The sheet host for every tool panel.
struct ElectronicsToolSheet: View {
    @Bindable var model: EditorModel
    let kind: ElectronicsTools.Sheet
    private var ec: ElectronicsWorkspace { model.electronics }
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            switch kind {
            case .drc: DrcPanel(model: model)
            case .erc: ErcPanel(model: model)
            case .footprint: FootprintEditorPanel(model: model)
            case .symbol: SymbolEditorPanel(model: model)
            case .length: LengthTuningPanel(model: model)
            case .stackup: LayerStackPanel(model: model)
            case .simulate: SimulationPanel(model: model)
            case .enclosure: EnclosureFitPanel(model: model)
            }
            if let e = ec.tools.error { Text(e).font(.callout).foregroundStyle(.red).textSelection(.enabled) }
            HStack { Spacer(); Button("Done") { GlassUI.shared.closePanel() }.keyboardShortcut(.defaultAction) }
        }
    }
}

private func stat(_ label: String, _ value: Any?, good: Bool? = nil) -> some View {
    VStack(alignment: .leading, spacing: 2) {
        Text("\(Int(ecNumber(value)))").font(.title2.monospacedDigit())
            .foregroundStyle(good == nil ? Color.primary : good! ? Color.green : Color.red)
        Text(label).font(.caption).foregroundStyle(.secondary)
    }
}

struct DrcPanel: View {
    @Bindable var model: EditorModel
    private var t: ElectronicsTools { model.electronics.tools }
    var body: some View {
        let r = t.drc ?? [:]
        // Geometry-clean with open nets is not a board you can fabricate;
        // the export itself refuses it, so the panel mustn't offer it.
        let open = Int(ecNumber(r["unrouted"]))
        let clean = (r["clean"] as? Bool ?? false) && open == 0
        Text("Route-attributable counts only what routing introduced, measured against the same board with every trace and via removed. Gerber export requires it to be zero.")
            .font(.callout).foregroundStyle(.secondary)
        HStack(spacing: 28) {
            stat("route-attributable", r["route_attributable"], good: clean)
            stat("total", r["total"])
            stat("unrouted floor", r["baseline_total"])
            stat("open nets", r["unrouted"], good: ecNumber(r["unrouted"]) == 0)
        }
        Label(clean ? "Clean — ready for Gerber export" : (r["clean"] as? Bool ?? false) ? "\(open) net\(open == 1 ? "" : "s") still open — route them before export" : "Not clean — run Fix DRC", systemImage: clean ? "checkmark.seal.fill" : "exclamationmark.triangle.fill")
            .foregroundStyle(clean ? .green : .orange)
        List(Array(ecRows(r["findings"]).prefix(300).enumerated()), id: \.offset) { _, f in
            HStack(alignment: .firstTextBaseline) {
                Button { model.electronics.focus(on: CGPoint(x: ecNumber(f["x"]), y: ecNumber(f["y"]))); GlassUI.shared.closePanel() } label: {
                    Image(systemName: "scope")
                }.buttonStyle(.borderless).help("Show on the board")
                Text(f["rule"] as? String ?? "").font(.caption.monospaced()).frame(width: 110, alignment: .leading)
                Text(f["message"] as? String ?? "").font(.caption).lineLimit(2)
                Spacer()
                Text(String(format: "%.2f, %.2f", ecNumber(f["x"]), ecNumber(f["y"]))).font(.caption2.monospacedDigit()).foregroundStyle(.secondary)
            }
        }.frame(height: 220)
        if let s = t.fixSummary {
            DisclosureGroup("Fix DRC report") { ScrollView { Text(s).font(.caption.monospaced()).textSelection(.enabled) }.frame(height: 120) }
        }
        HStack {
            Button("Re-run DRC") { t.runDrc(model.electronics, model) }
            Button("Fix DRC") { t.fixDrc(model.electronics, model) }.disabled(clean)
            Spacer()
            Button("Export Gerbers…") { t.exportGerbers(model.electronics, model) }.disabled(!clean)
        }.disabled(t.busy != nil)
    }
}

struct LengthTuningPanel: View {
    @Bindable var model: EditorModel
    private var t: ElectronicsTools { model.electronics.tools }
    var body: some View {
        @Bindable var t = t
        Text("Select nets to match; shorter nets get clearance-checked meanders up to the target (default: the longest selected).")
            .font(.callout).foregroundStyle(.secondary)
        List(t.lengths, id: \.0) { row in
            Toggle(isOn: Binding(get: { t.tuneNets.contains(row.0) },
                                 set: { if $0 { t.tuneNets.insert(row.0) } else { t.tuneNets.remove(row.0) } })) {
                HStack {
                    Text(row.0).font(.callout.monospaced())
                    Spacer()
                    Text(String(format: "%.2f mm", row.1)).monospacedDigit().foregroundStyle(.secondary)
                    if let done = t.tuneResult.first(where: { $0["net"] as? String == row.0 }) {
                        Text(String(format: "→ %.2f", ecNumber(done["length_after"])))
                            .foregroundStyle(done["matched"] as? Bool == true ? .green : .orange)
                    }
                }
            }
        }.frame(height: 240)
        HStack {
            Text("Target (mm)")
            TextField("longest", value: $t.tuneTarget, format: .number).frame(width: 80)
            Text("Tolerance")
            TextField("", value: $t.tuneTolerance, format: .number).frame(width: 60)
            Spacer()
            Button("Tune \(t.tuneNets.count) Net(s)") { t.tune(model.electronics, model) }
                .disabled(t.tuneNets.isEmpty || t.busy != nil)
        }
        ForEach(Array(t.tuneResult.enumerated()), id: \.offset) { _, n in
            if let why = n["skip_reason"] as? String { Text("\(n["net"] as? String ?? ""): \(why)").font(.caption).foregroundStyle(.orange) }
        }
    }
}

struct LayerStackPanel: View {
    @Bindable var model: EditorModel
    private var ec: ElectronicsWorkspace { model.electronics }
    static let order = ["FCu", "In1Cu", "In2Cu", "In3Cu", "In4Cu", "In5Cu", "In6Cu", "BCu"]
    var body: some View {
        let layers = ecRows((ec.board(model)["stackup"] as? ECObject)?["layers"])
        let copper = layers.filter { ($0["layer"] as? String ?? "").hasSuffix("Cu") }
        Text("\(copper.count) copper layers · board \(String(format: "%.2f", ecNumber((ec.board(model)["outline"] as? ECObject)?["thickness"], 1.6))) mm")
            .foregroundStyle(.secondary)
        Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 6) {
            GridRow { Text("Layer").bold(); Text("Copper (mm)").bold(); Text("Dielectric below (mm)").bold() }
            ForEach(Array(layers.enumerated()), id: \.offset) { i, l in
                GridRow {
                    Text(l["layer"] as? String ?? "?").font(.callout.monospaced())
                    TextField("", value: binding(i, "copperThickness"), format: .number).frame(width: 90)
                    TextField("", value: binding(i, "dielectricThickness"), format: .number).frame(width: 90)
                }
            }
        }
        HStack {
            Picker("Copper layers", selection: Binding(get: { copper.count }, set: { setCount($0) })) {
                ForEach([2, 4, 6, 8], id: \.self) { Text("\($0) layers").tag($0) }
            }.frame(width: 200)
            Spacer()
        }
    }
    private func binding(_ i: Int, _ key: String) -> Binding<Double> {
        Binding(get: { ecNumber(ecRows((ec.board(model)["stackup"] as? ECObject)?["layers"])[safe: i]?[key]) },
                set: { v in
                    ec.mutateBoard(model) { b in
                        var s = b["stackup"] as? ECObject ?? [:], rows = ecRows(s["layers"])
                        guard rows.indices.contains(i), v >= 0 else { return }
                        rows[i][key] = v; s["layers"] = rows; b["stackup"] = s
                    }
                })
    }
    /// Rebuild the copper stack with `n` layers, keeping thicknesses of layers
    /// that survive and spreading the board thickness over the dielectrics.
    private func setCount(_ n: Int) {
        let names = n == 2 ? ["FCu", "BCu"] : Array(Self.order.prefix(n - 1)) + ["BCu"]
        ec.mutateBoard(model) { b in
            var s = b["stackup"] as? ECObject ?? [:]
            let old = ecRows(s["layers"])
            let thick = ecNumber((b["outline"] as? ECObject)?["thickness"], 1.6)
            let cu = 0.035, diel = max(0.05, (thick - Double(n) * cu) / Double(n - 1))
            s["layers"] = names.enumerated().map { i, name -> ECObject in
                var row = old.first(where: { $0["layer"] as? String == name }) ?? ["layer": name, "copperThickness": cu]
                if i < names.count - 1 { row["dielectricThickness"] = diel } else { row.removeValue(forKey: "dielectricThickness") }
                return row
            }
            b["stackup"] = s
        }
    }
}

extension Array {
    subscript(safe i: Int) -> Element? { indices.contains(i) ? self[i] : nil }
}

struct EnclosureFitPanel: View {
    @Bindable var model: EditorModel
    private var t: ElectronicsTools { model.electronics.tools }
    var body: some View {
        @Bindable var t = t
        let parts = t.partNames(model)
        Text("Checks the board against the enclosure's cavity: wall clearance, lid clearance, mounting holes over standoffs, and connectors at wall openings.")
            .font(.callout).foregroundStyle(.secondary)
        HStack {
            Picker("Enclosure part", selection: $t.enclosurePart) {
                ForEach(Array(parts.enumerated()), id: \.offset) { i, n in Text(n).tag(i) }
            }.frame(width: 320)
            Spacer()
            Button("Enclose Board") { t.encloseBoard(model.electronics, model) }
            Button("Run Fit Check") { t.fitCheck(model.electronics, model) }.buttonStyle(.borderedProminent)
        }.disabled(t.busy != nil)
        if let fit = t.fit {
            let ok = fit["ok"] as? Bool ?? false
            Label(fit["summary"] as? String ?? "", systemImage: ok ? "checkmark.seal.fill" : "xmark.octagon.fill")
                .foregroundStyle(ok ? .green : .red)
            List(Array(ecRows(fit["checks"]).enumerated()), id: \.offset) { _, c in
                let status = c["status"] as? String ?? ""
                HStack(alignment: .firstTextBaseline) {
                    Image(systemName: status == "pass" ? "checkmark.circle" : status == "fail" ? "xmark.circle" : "exclamationmark.circle")
                        .foregroundStyle(status == "pass" ? .green : status == "fail" ? .red : .orange)
                    VStack(alignment: .leading) {
                        Text(c["label"] as? String ?? "").bold()
                        Text(c["detail"] as? String ?? "").font(.caption).foregroundStyle(.secondary)
                    }
                }
            }.frame(height: 200)
        }
    }
}

struct SimulationPanel: View {
    @Bindable var model: EditorModel
    private var t: ElectronicsTools { model.electronics.tools }
    var body: some View {
        @Bindable var t = t
        let nets = ((model.electronics.schematic(model)["nets"] as? [String: Any]) ?? [:]).keys.sorted()
        Text("DC operating point (and an optional transient) of the schematic. Parts map by reference: R, C, L, V (source, value in volts), I, D. Connectors and test points are left open.")
            .font(.callout).foregroundStyle(.secondary)
        HStack {
            Picker("Ground net", selection: $t.groundNet) {
                Text("GND (by name)").tag("")
                ForEach(nets, id: \.self) { Text($0).tag($0) }
            }.frame(width: 260)
            Text("Transient (s)")
            TextField("off", value: $t.tstop, format: .number).frame(width: 80)
            Spacer()
            Button("Run") { t.simulate(model.electronics, model) }.buttonStyle(.borderedProminent).disabled(t.busy != nil)
        }
        if let sim = t.sim {
            let dc = sim["dc"] as? ECObject ?? [:]
            Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 4) {
                GridRow { Text("Net").bold(); Text("DC (V)").bold(); Text("Final (V)").bold() }
                ForEach(dc.keys.sorted(), id: \.self) { n in
                    let tr = ((sim["transient"] as? ECObject)?["nets"] as? ECObject)?[n] as? [Any]
                    GridRow {
                        Text(n).font(.callout.monospaced())
                        Text(String(format: "%.4f", ecNumber(dc[n]))).monospacedDigit()
                        Text(tr.map { String(format: "%.4f", ecNumber($0.last)) } ?? "—").monospacedDigit().foregroundStyle(.secondary)
                    }
                }
            }
            if let stub = sim["stubbed"] as? [String], !stub.isEmpty {
                Text("Left open: \(stub.joined(separator: ", "))").font(.caption).foregroundStyle(.secondary)
            }
        }
        ForEach(t.erc, id: \.self) { Text("ERC: \($0)").font(.caption).foregroundStyle(.orange) }
    }
}


/// Electrical rule check results, each with a jump to its place on the sheet.
struct ErcPanel: View {
    @Bindable var model: EditorModel
    var body: some View {
        let t = model.electronics.tools
        let errors = t.ercFindings.filter { ($0["severity"] as? String ?? "").lowercased() == "error" }.count
        HStack(spacing: 28) {
            VStack(alignment: .leading) { Text("\(errors)").font(.title2.monospacedDigit()).foregroundStyle(errors == 0 ? .green : .red); Text("errors").font(.caption) }
            VStack(alignment: .leading) { Text("\(t.ercFindings.count - errors)").font(.title2.monospacedDigit()); Text("warnings").font(.caption) }
        }
        List(Array(t.ercFindings.prefix(300).enumerated()), id: \.offset) { _, f in
            HStack(alignment: .firstTextBaseline) {
                if f["position"] is ECObject {
                    Button {
                        model.electronics.focus(on: ecPoint(f["position"]), minZoom: 2)
                        GlassUI.shared.closePanel()
                    } label: { Image(systemName: "scope") }.buttonStyle(.borderless).help("Show on the sheet")
                }
                Text(f["severity"] as? String ?? "").font(.caption.monospaced()).frame(width: 60, alignment: .leading)
                Text(f["message"] as? String ?? "").font(.caption).lineLimit(3)
            }
        }.frame(height: 280)
    }
}


/// Edit the selected part's land pattern: pad numbers, positions, sizes, shape.
struct FootprintEditorPanel: View {
    @Bindable var model: EditorModel
    private var ec: ElectronicsWorkspace { model.electronics }
    @State private var rowCount = 4
    @State private var pitch = 1.27
    private var fp: ECObject { ecRows(ec.board(model)["footprints"]).first { $0["ref"] as? String == ec.selectedRef } ?? [:] }
    private func set(_ i: Int, _ change: @escaping (inout ECObject) -> Void) {
        ec.editFootprint(model) { f in var p = ecRows(f["pads"]); guard p.indices.contains(i) else { return }; change(&p[i]); f["pads"] = p }
    }
    private func num(_ value: Double, _ apply: @escaping (Double) -> Void) -> some View {
        TextField("", value: Binding(get: { value }, set: apply), format: .number.precision(.fractionLength(0...3))).frame(width: 56).multilineTextAlignment(.trailing)
    }
    var body: some View {
        let pads = ecRows(fp["pads"])
        Text(fp["footprintName"] as? String ?? (ec.selectedRef ?? "")).font(.caption).foregroundStyle(.secondary).lineLimit(1)
        Canvas { ctx, size in
            let pts = pads.map { ecPoint($0["position"]) }
            let span = max(2, (pts.map { max(abs($0.x), abs($0.y)) }.max() ?? 1) * 2 + 3)
            let k = min(size.width, size.height) / span
            for pad in pads {
                let p = ecPoint(pad["position"]), s = pad["shape"] as? ECObject ?? [:], d = ecNumber(s["diameter"], 0.6)
                let w = ecNumber(s["width"], d) * k, h = ecNumber(s["height"], d) * k
                let c = CGPoint(x: size.width / 2 + p.x * k, y: size.height / 2 + p.y * k)
                let r = CGRect(x: c.x - w / 2, y: c.y - h / 2, width: w, height: h)
                ctx.fill(s["type"] as? String == "Circle" ? Path(ellipseIn: r) : Path(roundedRect: r, cornerRadius: 1), with: .color(BoardLayers.color((pad["layers"] as? [String])?.first ?? "FCu")))
                ctx.draw(Text(pad["number"] as? String ?? "").font(.system(size: 9)).foregroundStyle(.white), at: c)
            }
        }.frame(height: 150).background(Color.black.opacity(0.25))
        List(pads.indices, id: \.self) { i in
            let pad = pads[i], shape = pad["shape"] as? ECObject ?? [:], at = ecPoint(pad["position"]), d = ecNumber(shape["diameter"], 0.6)
            HStack(spacing: 4) {
                TextField("#", text: Binding(get: { pad["number"] as? String ?? "" }, set: { v in set(i) { $0["number"] = v } })).frame(width: 34)
                num(Double(at.x)) { v in set(i) { $0["position"] = ecJSON(CGPoint(x: v, y: at.y)) } }
                num(Double(at.y)) { v in set(i) { $0["position"] = ecJSON(CGPoint(x: at.x, y: v)) } }
                num(ecNumber(shape["width"], d)) { v in set(i) { var s = $0["shape"] as? ECObject ?? [:]; s["width"] = v; $0["shape"] = s } }
                num(ecNumber(shape["height"], d)) { v in set(i) { var s = $0["shape"] as? ECObject ?? [:]; s["height"] = v; $0["shape"] = s } }
                Button { ec.editFootprint(model) { f in var p = ecRows(f["pads"]); if p.indices.contains(i) { p.remove(at: i) }; f["pads"] = p } } label: { Image(systemName: "trash") }.buttonStyle(.borderless)
            }.font(.caption.monospacedDigit())
        }.frame(height: 200)
        HStack {
            Stepper("Row of \(rowCount)", value: $rowCount, in: 1...64)
            TextField("Pitch", value: $pitch, format: .number).frame(width: 60)
            Button("Add Row") {
                let n = rowCount, pitch = pitch
                ec.editFootprint(model) { f in
                    var p = ecRows(f["pads"])
                    let y = (p.map { ecPoint($0["position"]).y }.max() ?? -pitch) + pitch
                    let start = p.count
                    for k in 0..<n {
                        let x = (Double(k) - Double(n - 1) / 2) * pitch
                        p.append(["number": "\(start + k + 1)", "padType": "SMD", "shape": ["type": "Rect", "width": pitch * 0.6, "height": 1.2], "position": ["x": x, "y": y], "rotation": 0, "layers": ["FCu", "FMask", "FPaste"]])
                    }
                    f["pads"] = p
                }
            }
        }
    }
}

/// Edit the selected symbol's pins: number, name, electrical type, position.
struct SymbolEditorPanel: View {
    @Bindable var model: EditorModel
    private var ec: ElectronicsWorkspace { model.electronics }
    private static let types = ["Input", "Output", "Bidirectional", "Passive", "PowerInput", "PowerOutput", "OpenCollector", "NotConnected", "Free"]
    private var symbol: ECObject { ecRows(ec.schematic(model)["components"]).first { $0["ref"] as? String == ec.selectedRef } ?? [:] }
    private func set(_ i: Int, _ change: @escaping (inout ECObject) -> Void) {
        ec.editSymbol(model) { c in var p = ecRows(c["pins"]); guard p.indices.contains(i) else { return }; change(&p[i]); c["pins"] = p }
    }
    var body: some View {
        let pins = ecRows(symbol["pins"])
        List(pins.indices, id: \.self) { i in
            let pin = pins[i], at = ecPoint(pin["position"])
            HStack(spacing: 4) {
                TextField("#", text: Binding(get: { pin["number"] as? String ?? "" }, set: { v in set(i) { $0["number"] = v } })).frame(width: 36)
                TextField("Name", text: Binding(get: { pin["name"] as? String ?? "" }, set: { v in set(i) { $0["name"] = v } })).frame(width: 90)
                Picker("", selection: Binding(get: { pin["pin_type"] as? String ?? "Passive" }, set: { v in set(i) { $0["pin_type"] = v } })) {
                    ForEach(Self.types, id: \.self) { Text($0) }
                }.labelsHidden().frame(width: 120)
                TextField("", value: Binding(get: { Double(at.x) }, set: { v in set(i) { $0["position"] = ecJSON(CGPoint(x: v, y: at.y)) } }), format: .number).frame(width: 52)
                TextField("", value: Binding(get: { Double(at.y) }, set: { v in set(i) { $0["position"] = ecJSON(CGPoint(x: at.x, y: v)) } }), format: .number).frame(width: 52)
                Button { ec.editSymbol(model) { c in var p = ecRows(c["pins"]); if p.indices.contains(i) { p.remove(at: i) }; c["pins"] = p } } label: { Image(systemName: "trash") }.buttonStyle(.borderless)
            }.font(.caption.monospacedDigit())
        }.frame(height: 300)
        HStack {
            Button("Add Pin") {
                ec.editSymbol(model) { c in
                    var p = ecRows(c["pins"])
                    p.append(["number": "\(p.count + 1)", "name": "\(p.count + 1)", "pin_type": "Passive", "position": ["x": -10.16, "y": Double(p.count) * 2.54]])
                    c["pins"] = p
                }
            }
            Button("Arrange Left and Right") {
                ec.editSymbol(model) { c in
                    var p = ecRows(c["pins"])
                    let left = (p.count + 1) / 2
                    for i in p.indices {
                        let onLeft = i < left, row = onLeft ? i : i - left, rows = onLeft ? left : p.count - left
                        p[i]["position"] = ["x": onLeft ? -10.16 : 10.16, "y": (Double(row) - Double(rows - 1) / 2) * 2.54]
                    }
                    c["pins"] = p
                }
            }
        }
    }
}
