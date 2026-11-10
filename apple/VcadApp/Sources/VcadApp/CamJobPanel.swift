import SwiftUI
import AppKit
import CVcadFFI

// Multi-tool CAM job (Fabricate ▸ CAM Job): web ToolLibrary / OperationList /
// CamPanel parity over vcad_cam_job — a tool library, operations bound to
// tools, stock-sim verification of the whole job (graded against the open
// document's part when "Verify against model" is on), and a post with tool
// changes. "Open in Manufacture" hands the program to the CNC workspace for
// toolpath preview and streaming; for Grbl, run the per-tool files.

struct CamTool: Identifiable, Equatable {
    let id = UUID()
    var number: Int
    var name = ""
    var kind = "flat_endmill"
    var diameter = 6.0
    var rpm = 12000.0
    var feed = 800.0
    var plunge = 200.0
    var json: [String: Any] {
        ["number": number, "name": name, "type": kind, "diameter": diameter, "rpm": rpm, "feed": feed, "plunge": plunge]
    }
    static let kinds = ["flat_endmill", "ball_endmill", "bull_endmill", "vbit", "drill", "face_mill"]
}

struct CamOp: Identifiable, Equatable {
    let id = UUID()
    var kind = "face"
    var tool = 1
    var depth = 1.0
    var stepdown = 1.0
    var stepover = 3.0
    var x = 0.0, y = 0.0, width = 0.0, height = 0.0
    var cx = 20.0, cy = 15.0, radius = 6.0
    var json: [String: Any] {
        ["kind": kind, "tool": tool, "depth": depth, "stepdown": stepdown, "stepover": stepover,
         "x": x, "y": y, "width": width, "height": height, "cx": cx, "cy": cy, "radius": radius]
    }
    static let kinds = ["face", "pocket", "circle_pocket", "profile"]
    static func label(_ k: String) -> String {
        switch k {
        case "pocket": return "Rect pocket"
        case "circle_pocket": return "Circle pocket"
        case "profile": return "Outer profile"
        default: return "Face"
        }
    }
}

@MainActor @Observable
final class CamJobStore {
    var tools: [CamTool] = [
        CamTool(number: 1, name: "6 mm flat", diameter: 6, rpm: 12000, feed: 800, plunge: 200),
        CamTool(number: 2, name: "1/8\" flat", diameter: 3.175, rpm: 16000, feed: 500, plunge: 120),
    ]
    var ops: [CamOp] = [
        CamOp(kind: "face", tool: 1, depth: 1, stepdown: 1, stepover: 4),
        CamOp(kind: "circle_pocket", tool: 2, depth: 4, stepdown: 1, stepover: 1.2, cx: 20, cy: 15, radius: 7),
    ]
    var stockW = 40.0
    var stockH = 30.0
    var stockD = 10.0
    var clearance = 5.0
    var post = "grbl"
    var verifyAgainstModel = false
    var busy = false
    var error: String?
    var result: [String: Any]?
    /// Inputs the current `result` was generated from; any edit since makes it stale.
    private(set) var resultInputs: String?

    /// Every job input that affects the result (stock, tools, ops, post,
    /// verify toggle), as a stable fingerprint — ids excluded.
    var inputsFingerprint: String {
        let payload: [String: Any] = [
            "stock": [stockW, stockH, stockD], "clearance": clearance, "post": post,
            "verify": verifyAgainstModel, "tools": tools.map(\.json), "ops": ops.map(\.json),
        ]
        let data = (try? JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])) ?? Data()
        return String(decoding: data, as: UTF8.self)
    }

    /// True when a result is on screen but the inputs changed after it was generated.
    var isStale: Bool { result != nil && resultInputs != inputsFingerprint }

    func loadDefaultLibrary() {
        guard let r = try? FabFFI.take(vcad_cam_tool_library()), let list = r["tools"] as? [[String: Any]] else { return }
        tools = list.map {
            CamTool(number: ($0["number"] as? Int) ?? 1, name: $0["name"] as? String ?? "",
                    kind: $0["type"] as? String ?? "flat_endmill", diameter: fabNum($0["diameter"]),
                    rpm: fabNum($0["rpm"]), feed: fabNum($0["feed"]), plunge: fabNum($0["plunge"]))
        }
    }

    func run(_ model: EditorModel) async {
        busy = true; error = nil
        defer { busy = false }
        let inputs = inputsFingerprint
        var req: [String: Any] = [
            "name": model.documentName.isEmpty ? "vcad job" : model.documentName,
            "stock": ["width": stockW, "height": stockH, "depth": stockD],
            "tools": tools.map(\.json), "operations": ops.map(\.json),
            "clearance": clearance, "post": post, "verify": true,
        ]
        if verifyAgainstModel {
            guard let doc = model.documentJSON else { error = "No document is open."; return }
            req["target"] = ["doc": doc, "part": model.selectedPartIndex ?? 0]
        }
        do {
            result = try await FabFFI.off(req) { try FabFFI.call1(vcad_cam_job, $0) }
            resultInputs = inputs
        } catch { self.error = error.localizedDescription; result = nil; resultInputs = nil }
    }

    /// The stock-sim headline; a stale result never claims a pass or fail.
    static func verdict(pass: Bool, graded: Bool, stale: Bool) -> String {
        if stale { return pass ? "Last run passed — inputs changed since" : "Last run failed — inputs changed since" }
        return pass ? (graded ? "Verified against the part" : "No floor breach (ungraded)") : "Verification failed"
    }

    func openInManufacture(_ model: EditorModel) {
        guard let code = result?["gcode"] as? String else { return }
        do {
            try model.cnc.importProgram(code, name: "vcad-job.nc")
            model.workspace = .manufacture
        } catch { self.error = error.localizedDescription }
    }
}

struct CamJobPanel: View {
    @Bindable var model: EditorModel
    @Bindable var store: CamJobStore

    var body: some View {
        Eyebrow("Stock (top = Z0)")
        HStack {
            FabField(label: "W", value: $store.stockW, digits: 1)
            FabField(label: "H", value: $store.stockH, digits: 1)
            FabField(label: "D", value: $store.stockD, digits: 1)
        }.font(.caption)
        .onAppear {
            // Untouched template stock (40 × 30 × 10) becomes the part's own
            // size, so a first Generate cuts the part on screen, not a template.
            let s = model.sizeMM
            if store.stockW == 40, store.stockH == 30, store.stockD == 10, s.x > 0, s.y > 0, s.z > 0 {
                store.stockW = (Double(s.x) * 10).rounded() / 10
                store.stockH = (Double(s.y) * 10).rounded() / 10
                store.stockD = (Double(s.z) * 10).rounded() / 10
            }
        }
        toolLibrary
        operations
        Picker("Post", selection: $store.post) {
            Text("Grbl (M0 tool change)").tag("grbl")
            Text("LinuxCNC (T M6)").tag("linuxcnc")
        }
        Toggle("Verify against model", isOn: $store.verifyAgainstModel)
            .help("Grade the simulated stock against the selected part (sitting on the stock bottom, corner at the origin)")
        Button {
            // Commit any field still being edited before reading the inputs.
            NSApp.keyWindow?.makeFirstResponder(nil)
            Task { await store.run(model) }
        } label: {
            if store.busy { ProgressView().controlSize(.small) } else { Text("Generate & Verify") }
        }
        .keyboardShortcut(.return, modifiers: [.command])
        .disabled(store.busy)
        if let e = store.error { Text(e).font(.caption).foregroundStyle(.red).textSelection(.enabled) }
        if let r = store.result {
            if store.isStale {
                Label("Out of date — Generate again", systemImage: "arrow.clockwise")
                    .font(.caption).foregroundStyle(.orange)
            }
            VStack(alignment: .leading, spacing: Theme.Space.xs) { results(r) }
                .opacity(store.isStale ? 0.4 : 1)
                .saturation(store.isStale ? 0 : 1)
        }
    }

    private var toolLibrary: some View {
        VStack(alignment: .leading, spacing: Theme.Space.xs) {
            HStack {
                Eyebrow("Tool library")
                Spacer()
                Menu {
                    Button("Add tool") {
                        store.tools.append(CamTool(number: (store.tools.map(\.number).max() ?? 0) + 1, diameter: 3))
                    }
                    Button("Load default library") { store.loadDefaultLibrary() }
                } label: { Image(systemName: "plus") }
                .menuStyle(.borderlessButton).fixedSize()
            }
            ForEach($store.tools) { $t in
                VStack(alignment: .leading, spacing: 2) {
                    HStack {
                        Text("T\(t.number)").font(.caption.monospacedDigit().bold())
                        TextField("name", text: $t.name).font(.caption)
                        Button { let id = t.id; store.tools.removeAll { $0.id == id } } label: { Image(systemName: "minus.circle") }
                            .buttonStyle(.borderless).disabled(store.tools.count <= 1)
                    }
                    Picker("", selection: $t.kind) {
                        ForEach(CamTool.kinds, id: \.self) { Text($0.replacingOccurrences(of: "_", with: " ")).tag($0) }
                    }.labelsHidden().font(.caption)
                    HStack {
                        FabField(label: "Ø", value: $t.diameter, digits: 3)
                        FabField(label: "S", value: $t.rpm, digits: 0)
                    }.font(.caption)
                    HStack {
                        FabField(label: "F", value: $t.feed, digits: 0)
                        FabField(label: "Fz", value: $t.plunge, digits: 0)
                    }.font(.caption)
                }
                .padding(6).background(RoundedRectangle(cornerRadius: 6).fill(.quaternary.opacity(0.25)))
            }
        }
    }

    private var operations: some View {
        VStack(alignment: .leading, spacing: Theme.Space.xs) {
            HStack {
                Eyebrow("Operations")
                Spacer()
                Button { store.ops.append(CamOp(tool: store.tools.first?.number ?? 1)) } label: { Image(systemName: "plus") }
                    .buttonStyle(.borderless)
            }
            ForEach(Array($store.ops.enumerated()), id: \.element.id) { i, $op in
                VStack(alignment: .leading, spacing: 2) {
                    HStack {
                        Text("\(i + 1).").font(.caption.monospacedDigit())
                        Picker("", selection: $op.kind) {
                            ForEach(CamOp.kinds, id: \.self) { Text(CamOp.label($0)).tag($0) }
                        }.labelsHidden()
                        Picker("", selection: $op.tool) {
                            ForEach(store.tools) { Text("T\($0.number)").tag($0.number) }
                        }.labelsHidden().frame(width: 64)
                        // Read the id first: `op` is a binding into `store.ops`, and reading it
                        // inside removeAll overlaps the mutation (a fatal exclusivity conflict).
                        Button { let id = op.id; store.ops.removeAll { $0.id == id } } label: { Image(systemName: "minus.circle") }
                            .buttonStyle(.borderless).disabled(store.ops.count <= 1)
                    }
                    HStack {
                        FabField(label: "Depth", value: $op.depth)
                        FabField(label: "Step ↓", value: $op.stepdown)
                        FabField(label: "Step →", value: $op.stepover)
                    }.font(.caption)
                    if op.kind == "circle_pocket" {
                        HStack {
                            FabField(label: "X", value: $op.cx, digits: 1)
                            FabField(label: "Y", value: $op.cy, digits: 1)
                            FabField(label: "R", value: $op.radius, digits: 2)
                        }.font(.caption)
                    } else if op.kind != "face" {
                        HStack {
                            FabField(label: "X", value: $op.x, digits: 1)
                            FabField(label: "Y", value: $op.y, digits: 1)
                        }.font(.caption)
                        HStack {
                            FabField(label: "W", value: $op.width, digits: 1)
                            FabField(label: "H", value: $op.height, digits: 1)
                        }.font(.caption)
                    }
                }
                .padding(6).background(RoundedRectangle(cornerRadius: 6).fill(.quaternary.opacity(0.25)))
            }
        }
    }

    @ViewBuilder private func results(_ r: [String: Any]) -> some View {
        let ops = r["operations"] as? [[String: Any]] ?? []
        Eyebrow("Toolpaths")
        ForEach(Array(ops.enumerated()), id: \.offset) { _, o in
            Text("\(CamOp.label(o["kind"] as? String ?? "")) · T\(o["tool"] as? Int ?? 0) · \(fabFmt(fabNum(o["cuttingLengthMm"]), 0)) mm · \(fabDuration(fabNum(o["estimatedSeconds"])))")
                .font(.caption.monospacedDigit())
        }
        // Plan view of the cutting moves, coloured per tool.
        let moves = r["moves"] as? [[String: Any]] ?? []
        let byTool = Dictionary(grouping: moves.filter { ($0["rapid"] as? Bool) == false }) { $0["tool"] as? Int ?? 0 }
        let polylines = byTool.keys.sorted().map { k in byTool[k]!.compactMap { m -> [Double]? in
            guard let to = m["to"] as? [NSNumber] else { return nil }
            return [to[0].doubleValue, to[1].doubleValue]
        } }
        let stock = [[0.0, 0.0], [store.stockW, 0], [store.stockW, store.stockH], [0, store.stockH]]
        FabRingsCanvas(closed: [stock], open: polylines.dropLast().map { $0 }, accent: polylines.last.map { [$0] } ?? [], height: 150)
        if let v = r["verification"] as? [String: Any] { verification(v) }
        Text("\(r["toolChanges"] as? Int ?? 0) tool change(s) · \(r["post"] as? String ?? "")").font(.caption)
        HStack {
            Button("Save G-code") {
                do { try FabFFI.save(r["gcode"] as? String ?? "", name: "vcad-job.nc", ext: "nc") }
                catch { store.error = error.localizedDescription }
            }
            Button("Per-tool files") {
                for p in r["programs"] as? [[String: Any]] ?? [] {
                    do { try FabFFI.save(p["gcode"] as? String ?? "", name: "vcad-job-T\(p["tool"] as? Int ?? 0).nc", ext: "nc") }
                    catch { store.error = error.localizedDescription }
                }
            }
            .help("One program per tool — recommended on Grbl")
        }
        Button("Open in Manufacture") { store.openInManufacture(model) }
    }

    @ViewBuilder private func verification(_ v: [String: Any]) -> some View {
        let pass = (v["pass"] as? Bool) ?? false
        let graded = (v["graded"] as? Bool) ?? false
        let stale = store.isStale
        Eyebrow("Stock sim")
        Label(CamJobStore.verdict(pass: pass, graded: graded, stale: stale),
              systemImage: stale ? "clock.arrow.circlepath" : (pass ? "checkmark.seal" : "xmark.octagon"))
            .font(.caption).foregroundStyle(stale ? Color.secondary : (pass ? Color.green : Color.red))
        Text("removed \(fabFmt(fabNum(v["removedVolumeMm3"]), 0)) of \(fabFmt(fabNum(v["stockVolumeMm3"]), 0)) mm³ · cell \(fabFmt(fabNum(v["resolution"]), 2)) mm")
            .font(.caption2.monospacedDigit()).foregroundStyle(.secondary)
        if let s = v["stock"] as? [String: Any] {
            let g = s["gouge"] as? [String: Any] ?? [:], x = s["excess"] as? [String: Any] ?? [:]
            Text("gouge: \(g["violation_count"] as? Int ?? 0) (worst \(fabFmt(fabNum(g["worst_depth"]), 2)) mm) · excess: \(x["violation_count"] as? Int ?? 0) (worst \(fabFmt(fabNum(x["worst_depth"]), 2)) mm)")
                .font(.caption2.monospacedDigit())
        }
    }
}
