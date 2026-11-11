import SwiftUI
import CVcadFFI

// 3D print (Fab ▸ 3D Print): part to printed part without leaving vcad.
// Four steps in one inspector — Orient, Slice, Printer, Print — with one
// primary button in the footer and Export beside it. Everything runs over
// vcad_print_call: orientation search, the same centre → slice → lint →
// printcheck → .gcode.3mf pipeline as `vcad print`, LAN discovery and pairing,
// live status, the fail-closed gated send, pause / resume / stop.
//
// A job reaches a printer only through `startPrint(confirmed: true)`, which
// only the confirm sheet calls; without it the engine runs the gate as a dry
// check and uploads nothing.

enum PrintStep: Int, CaseIterable, Identifiable {
    case orient, slice, printer, print
    var id: Int { rawValue }
    var title: String {
        switch self {
        case .orient: "Orient"
        case .slice: "Slice"
        case .printer: "Printer"
        case .print: "Print"
        }
    }
}

enum PrintQuality: String, CaseIterable, Identifiable {
    case fast = "Fast", standard = "Standard", strong = "Strong"
    var id: String { rawValue }
    /// (layer height, walls, infill fraction, pattern)
    var preset: (Double, Int, Double, String) {
        switch self {
        case .fast: (0.28, 2, 0.10, "grid")
        case .standard: (0.20, 3, 0.15, "grid")
        case .strong: (0.16, 5, 0.40, "gyroid")
        }
    }
    var summary: String {
        let p = preset
        return "\(fabFmt(p.0, 2)) mm · \(p.1) walls · \(Int(p.2 * 100))% \(p.3)"
    }
}

/// A candidate resting pose from the engine's orientation search.
struct PrintPose: Identifiable, Equatable {
    let id: Int
    let rotation: [[Double]]
    let supportFree: Bool
    let overhangCount: Int
    let supportVolume: Double
    let height: Double
    let printTime: Double?
    let grams: Double?
    /// False when the engine found the posed part overflows the printer's
    /// build volume (it can't slice). Defaults true when not estimated.
    var fits = true

    /// Short human name from the facet that rests on the plate.
    let name: String

    static func parse(_ v: [String: Any], index: Int) -> PrintPose? {
        guard let rot = v["rotation"] as? [[NSNumber]] else { return nil }
        let down = (v["down"] as? [NSNumber])?.map(\.doubleValue) ?? [0, 0, -1]
        let name: String
        if down[2] < -0.999 { name = "As modeled" }
        else if down[2] > 0.999 { name = "Flipped" }
        else if abs(down[2]) < 0.001 {
            // Every side pose read "On side", so a ranked list of three was
            // indistinguishable. Name the face that rests on the plate (Z-up,
            // Y forward: the front face looks down -Y).
            name = abs(down[0]) >= abs(down[1])
                ? (down[0] > 0 ? "On right side" : "On left side")
                : (down[1] > 0 ? "On back" : "On front")
        }
        else { name = "Tilted" }
        return PrintPose(id: index, rotation: rot.map { $0.map(\.doubleValue) },
                         supportFree: v["supportFree"] as? Bool ?? false,
                         overhangCount: (v["overhangCount"] as? NSNumber)?.intValue ?? 0,
                         supportVolume: fabNum(v["supportVolume"]), height: fabNum(v["height"]),
                         printTime: (v["printTimeSeconds"] as? NSNumber)?.doubleValue,
                         grams: (v["filamentGrams"] as? NSNumber)?.doubleValue,
                         fits: v["fits"] as? Bool ?? true, name: name)
    }
}

/// A saved (or simulated) printer.
struct SavedPrinter: Identifiable, Equatable, Hashable {
    var id: String { name }
    let name: String
    let model: String
    let ip: String
}

/// One AMS tray as the printer reports it.
struct AmsTray: Identifiable, Equatable {
    var id: Int { tray }
    let tray: Int
    let type: String?
    let color: String?
    let remaining: Int?
    var label: String { "\(tray + 1) · \(type ?? "Empty")" }
}

/// Live printer state.
struct PrinterSnapshot: Equatable {
    var state = "unknown"
    var ready = false
    var progress = 0.0
    var layer = 0, layers = 0
    var remainingMin = 0
    var nozzle = 0.0, bed = 0.0
    var trays: [AmsTray] = []
    var firmware: String?

    static func parse(_ v: [String: Any]) -> PrinterSnapshot {
        var s = PrinterSnapshot()
        s.state = v["state"] as? String ?? "unknown"
        s.ready = v["ready"] as? Bool ?? false
        s.progress = fabNum(v["progress"])
        s.layer = (v["layer"] as? NSNumber)?.intValue ?? 0
        s.layers = (v["layers"] as? NSNumber)?.intValue ?? 0
        s.remainingMin = (v["remainingMin"] as? NSNumber)?.intValue ?? 0
        s.nozzle = fabNum(v["nozzle"]); s.bed = fabNum(v["bed"])
        s.firmware = v["firmware"] as? String
        s.trays = (v["ams"] as? [[String: Any]] ?? []).map {
            AmsTray(tray: ($0["tray"] as? NSNumber)?.intValue ?? 0, type: $0["type"] as? String,
                    color: $0["color"] as? String, remaining: ($0["remaining"] as? NSNumber)?.intValue)
        }
        return s
    }
    var isActive: Bool { state == "printing" || state == "paused" || state == "preparing" }
    var stateLabel: String {
        switch state {
        case "idle": "Idle, ready"
        case "finished": "Finished, ready"
        case "printing": "Printing"
        case "paused": "Paused"
        case "preparing": "Preparing"
        default: state.capitalized
        }
    }
}

/// Printability warnings collapsed by kind: one line each, never a repeated list.
struct WarningGroup: Identifiable, Equatable {
    var id: String { kind }
    let kind: String
    let count: Int
    let faces: [Int]
    let message: String

    static func group(_ warnings: [[String: Any]]) -> [WarningGroup] {
        var order: [String] = []
        var byKind: [String: (Int, [Int], String)] = [:]
        for w in warnings {
            let kind = w["kind"] as? String ?? (w["message"] as? String ?? "issue")
            let faces = (w["face_indices"] as? [NSNumber])?.map(\.intValue) ?? []
            if byKind[kind] == nil { order.append(kind); byKind[kind] = (0, [], w["message"] as? String ?? kind) }
            byKind[kind]!.0 += 1
            byKind[kind]!.1 += faces
        }
        return order.map { k in
            let (n, faces, msg) = byKind[k]!
            return WarningGroup(kind: k, count: n, faces: faces, message: Self.sentence(kind: k, count: n, sample: msg))
        }
    }

    /// One sentence per group. Overhangs read as a count; anything else keeps
    /// its own message with a count when repeated.
    static func sentence(kind: String, count: Int, sample: String) -> String {
        let k = kind.lowercased()
        if k.contains("overhang") || sample.contains("supports required") {
            return count == 1 ? "1 face overhangs and needs support" : "\(count) faces overhang and need support"
        }
        let base = sample.components(separatedBy: " (").first ?? sample
        return count > 1 ? "\(base) (×\(count))" : base
    }
}

@MainActor @Observable
final class PrintStore {
    // Settings (wire names match the engine's `settings`).
    var printers: [(id: String, name: String)] = []
    var printer = "bambu_p1s"
    var material = "PLA"
    var layerHeight = 0.2
    var walls = 3
    var infillPct = 15.0
    var pattern = "grid"
    var supports = false
    var printTemp = 220
    var bedTemp = 55
    var quality: PrintQuality = .standard { didSet { applyQuality() } }

    var step: PrintStep = .orient
    var busy = false
    var error: String?

    // Orient
    var analysis: [String: Any]?
    var poses: [PrintPose] = []
    var pose = 0
    var modeledOverhangs = 0

    // Slice
    var slice: [String: Any]?
    var prepared: [String: Any]?
    var layer = 0
    var lastExport: String?

    // Printer
    var savedPrinters: [SavedPrinter] = []
    var device: String?
    var snapshot: PrinterSnapshot?
    var discovered: [[String: Any]] = []
    var discovering = false
    var amsTray: Int?
    /// Show the simulated printer (tests, previews; `VCAD_PRINT_DRY_RUN=1`).
    var includeDryRun = ProcessInfo.processInfo.environment["VCAD_PRINT_DRY_RUN"] == "1"

    // Print
    var confirming = false
    var sent: [String: Any]?
    private var poll: Task<Void, Never>?

    init() { applyQuality() }

    var groups: [WarningGroup] {
        WarningGroup.group((analysis?["dfm"] as? [String: Any])?["warnings"] as? [[String: Any]] ?? [])
    }
    var selectedPose: PrintPose? { poses.indices.contains(pose) ? poses[pose] : nil }
    var bestPose: Int? { poses.firstIndex { $0.supportFree && $0.fits } }

    private func applyQuality() {
        let p = quality.preset
        layerHeight = p.0; walls = p.1; infillPct = p.2 * 100; pattern = p.3
        invalidateSlice()
    }

    func invalidateSlice() { slice = nil; prepared = nil }

    var settings: [String: Any] {
        ["printer": printer, "material": material, "layer_height": layerHeight, "wall_count": walls,
         "infill_density": infillPct / 100, "infill_pattern": pattern,
         "support_enabled": supports || !(selectedPose?.supportFree ?? true),
         "print_temp": printTemp, "bed_temp": bedTemp,
         "first_layer_height": max(layerHeight, 0.2)]
    }

    /// Request fields shared by prepare / print / export.
    func jobRequest(name: String) -> [String: Any] {
        var r: [String: Any] = ["settings": settings, "name": name]
        if let p = selectedPose { r["rotation"] = p.rotation }
        return r
    }

    // MARK: engine

    static func engine(_ doc: Data?, _ req: [String: Any]) async throws -> [String: Any] {
        let box = FabFFI.Box(v: req)
        return try await FabFFI.off(box: box) { try FabFFI.call2(vcad_print_call, doc: doc, $0) }
    }

    private func run(_ doc: Data?, _ req: [String: Any]) async -> [String: Any]? {
        busy = true; error = nil
        defer { busy = false }
        do { return try await Self.engine(doc, req) } catch { self.error = error.localizedDescription; return nil }
    }

    private func doc(_ model: EditorModel) -> Data? {
        guard let d = AnalysisStore.documentData(model) else { error = "No document is open."; return nil }
        return d
    }

    func loadPrinters() {
        guard printers.isEmpty, let r = try? FabFFI.call2(vcad_print_call, doc: nil, ["op": "profiles"]),
              let list = r["profiles"] as? [[String: Any]] else { return }
        printers = list.compactMap { p in
            guard let id = p["id"] as? String else { return nil }
            return (id, p["name"] as? String ?? id)
        }
        if !printers.contains(where: { $0.id == printer }), let first = printers.first { printer = first.id }
    }

    // MARK: orient

    func orient(_ model: EditorModel) async { await orient(doc: doc(model), part: model.selectedPartIndex ?? 0) }

    func orient(doc: Data?, part: Int = 0) async {
        guard let doc else { return }
        analysis = await run(doc, ["op": "analyze", "part": part, "settings": settings])
        let o = await run(doc, ["op": "orient", "settings": settings, "max": 3])
        setPoses((o?["poses"] as? [[String: Any]]) ?? [])
        modeledOverhangs = ((o?["modeled"] as? [String: Any])?["overhangCount"] as? NSNumber)?.intValue ?? 0
        invalidateSlice()
    }

    /// Adopt the engine's ranked poses. A pose that overflows the build
    /// volume sinks to the bottom and is never pre-selected: the K1 PTO
    /// assembly's top pick laid it on its side (240 mm along Y on a 236 mm
    /// bed), and Slice then failed with "part is … fits 236 × 236 × 256".
    func setPoses(_ raw: [[String: Any]]) {
        let parsed = raw.compactMap { PrintPose.parse($0, index: 0) }
        let ordered = parsed.filter(\.fits) + parsed.filter { !$0.fits }
        poses = ordered.enumerated().map { i, p in
            PrintPose(id: i, rotation: p.rotation, supportFree: p.supportFree, overhangCount: p.overhangCount,
                      supportVolume: p.supportVolume, height: p.height, printTime: p.printTime, grams: p.grams,
                      fits: p.fits, name: p.name)
        }
        pose = poses.firstIndex { $0.name == "As modeled" && $0.fits } ?? poses.firstIndex(where: \.fits) ?? 0
    }

    /// "Orient to fix": the best support-free pose.
    func fixByOrienting() {
        if let b = bestPose { pose = b; invalidateSlice() }
    }

    // MARK: slice

    func sliceNow(_ model: EditorModel) async {
        let name = model.documentName.isEmpty ? "part" : model.documentName
        await sliceNow(doc: doc(model), name: name)
    }

    func sliceNow(doc: Data?, name: String) async {
        guard let doc else { return }
        var preview: [String: Any] = ["op": "slice", "settings": settings, "maxPreviewLayers": 400]
        if let p = selectedPose { preview["rotation"] = p.rotation }
        var req = jobRequest(name: name); req["op"] = "prepare"
        prepared = await run(doc, req)
        if prepared != nil { slice = await run(doc, preview) }
        layer = 0
    }

    var readyToPrint: Bool {
        guard let p = prepared else { return false }
        return (p["printcheckOk"] as? Bool ?? false) && (p["lintOk"] as? Bool ?? false)
    }

    // MARK: export

    func export(_ model: EditorModel, kind: String) async {
        let name = model.documentName.isEmpty ? "part" : model.documentName
        guard let doc = doc(model) else { return }
        var req = jobRequest(name: name)
        switch kind {
        case "gcode":
            guard let url = FabFFI.savePanel("\(name).gcode", ext: "gcode") else { return }
            req["op"] = "prepare"; req["gcodePath"] = url.path
        case "bambu":
            guard let url = FabFFI.savePanel("\(name).gcode.3mf", ext: "3mf") else { return }
            req["op"] = "prepare"; req["path"] = url.path
        default:
            guard let url = FabFFI.savePanel("\(name).3mf", ext: "3mf") else { return }
            req["op"] = "export_3mf"; req["path"] = url.path
        }
        if let r = await run(doc, req) { lastExport = (r["gcodePath"] ?? r["path"]) as? String }
    }

    // MARK: printer

    func refreshPrinters() async {
        let r = await run(nil, ["op": "printers", "includeDryRun": includeDryRun])
        savedPrinters = (r?["printers"] as? [[String: Any]] ?? []).compactMap {
            guard let n = $0["name"] as? String else { return nil }
            return SavedPrinter(name: n, model: $0["model"] as? String ?? "", ip: $0["ip"] as? String ?? "")
        }
        if device == nil || !savedPrinters.contains(where: { $0.name == device }) { device = savedPrinters.first?.name }
        if device != nil { await refreshStatus() }
    }

    func discover() async {
        discovering = true
        defer { discovering = false }
        let r = await run(nil, ["op": "discover", "timeoutMs": 3000])
        discovered = (r?["found"] as? [[String: Any]] ?? []).filter { ($0["saved"] as? Bool) != true }
    }

    /// Save a printer. The access code goes straight to the engine, which
    /// writes it to the owner-only printers.toml the `vcad` CLI also reads.
    func addPrinter(name: String, model: String, ip: String, serial: String, accessCode: String) async -> Bool {
        let r = await run(nil, ["op": "add_printer", "name": name, "model": model, "ip": ip,
                                "serial": serial, "access_code": accessCode])
        guard r != nil else { return false }
        device = name
        await refreshPrinters()
        return true
    }

    func refreshStatus() async {
        guard let device else { snapshot = nil; return }
        do {
            let r = try await Self.engine(nil, ["op": "printer_status", "printer": device])
            snapshot = PrinterSnapshot.parse(r)
            if amsTray == nil || !(snapshot?.trays.contains { $0.tray == amsTray } ?? false) {
                amsTray = snapshot?.trays.first { $0.type?.uppercased() == material.uppercased() }?.tray
            }
        } catch {
            snapshot = nil
            self.error = "\(device) is not responding: \(error.localizedDescription)"
        }
    }

    /// Why Print is disabled, or nil when it isn't.
    var printBlocker: String? {
        guard prepared != nil else { return "Slice first." }
        guard readyToPrint else { return "printcheck or the G-code linter refused this job." }
        guard device != nil else { return "Add a printer." }
        guard let s = snapshot else { return "Printer is not responding." }
        guard s.ready else { return "Printer is \(s.stateLabel.lowercased())." }
        if !s.trays.isEmpty {
            guard let t = amsTray, let tray = s.trays.first(where: { $0.tray == t }) else { return "Pick an AMS slot." }
            guard tray.type?.uppercased() == material.uppercased() else {
                return "Slot \(t + 1) holds \(tray.type ?? "nothing"), the job is \(material)."
            }
        }
        return nil
    }

    // MARK: print

    /// Upload and start. `confirmed` is true only from the confirm sheet;
    /// false runs every gate as a dry check and uploads nothing.
    @discardableResult
    func startPrint(doc: Data?, name: String, confirmed: Bool) async -> Bool {
        guard let doc, let device else { return false }
        var req = jobRequest(name: name)
        req["op"] = "print"; req["printer"] = device; req["confirm"] = confirmed
        if let t = amsTray { req["amsTray"] = t }
        guard let r = await run(doc, req) else { return false }
        guard confirmed else { return r["gatesPass"] as? Bool ?? false }
        sent = r
        step = .print
        await refreshStatus()
        return r["sent"] as? Bool ?? false
    }

    func startPrint(_ model: EditorModel) async {
        let name = model.documentName.isEmpty ? "part" : model.documentName
        await startPrint(doc: doc(model), name: name, confirmed: true)
        startPolling()
    }

    func control(_ action: String) async {
        guard let device else { return }
        _ = await run(nil, ["op": "control", "printer": device, "action": action])
        await refreshStatus()
    }

    func startPolling(every seconds: Double = 5) {
        poll?.cancel()
        poll = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(seconds))
                guard let self else { return }
                await self.refreshStatus()
                if let s = self.snapshot, !s.isActive { return }
            }
        }
    }

    func stopPolling() { poll?.cancel(); poll = nil }

    /// The footer's primary action for the current step.
    enum Primary: Equatable { case orientToFix, next, slice, print, pause, resume, again, none }
    var primary: Primary {
        switch step {
        case .orient:
            if let p = selectedPose, !p.supportFree, bestPose != nil { return .orientToFix }
            return .next
        case .slice: return prepared == nil ? .slice : .next
        case .printer: return .print
        case .print:
            switch snapshot?.state {
            case "printing", "preparing": return .pause
            case "paused": return .resume
            case "finished", "idle": return .again
            default: return .none
            }
        }
    }
}

// MARK: - Views

/// Apple-style grouped section: rounded fill, hairlines between rows.
private struct PrintGroup<Content: View>: View {
    let title: String?
    @ViewBuilder let content: Content
    init(_ title: String? = nil, @ViewBuilder content: () -> Content) {
        self.title = title; self.content = content()
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let title { Text(title).font(.caption.weight(.semibold)).foregroundStyle(.secondary).padding(.leading, 2) }
            Group(subviews: content) { rows in
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(rows.enumerated()), id: \.offset) { i, row in
                        row.padding(.vertical, 6)
                        if i < rows.count - 1 { Divider() }
                    }
                }
            }
            .padding(.horizontal, 10)
            .background(RoundedRectangle(cornerRadius: 8).fill(.quaternary.opacity(0.5)))
        }
    }
}

/// One-line status with a dot: green ok, orange attention.
private struct StatusLine: View {
    let ok: Bool
    let title: String
    var detail: String?
    /// Informational line: grey dot instead of green / orange.
    var neutral = false
    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Circle().fill(neutral ? Color.secondary : ok ? Color.green : Color.orange).frame(width: 6, height: 6).alignmentGuide(.firstTextBaseline) { $0[.bottom] - 1 }
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                if let detail { Text(detail).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true) }
            }
        }
        .font(.callout)
        .accessibilityElement(children: .combine)
    }
}

private func swatch(_ hex: String?) -> some View {
    let c: Color = {
        guard let h = hex?.trimmingCharacters(in: CharacterSet(charactersIn: "#")), h.count >= 6,
              let v = UInt32(h.prefix(6), radix: 16) else { return .clear }
        return Color(red: Double(v >> 16 & 0xFF) / 255, green: Double(v >> 8 & 0xFF) / 255, blue: Double(v & 0xFF) / 255)
    }()
    return Circle().fill(c).overlay(Circle().strokeBorder(.secondary.opacity(0.5), lineWidth: 0.5)).frame(width: 10, height: 10)
}

struct PrintWorkbench: View {
    @Bindable var model: EditorModel
    @Bindable var store: PrintStore
    @State private var pairing: [String: Any]?

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            Picker("Step", selection: $store.step) {
                ForEach(PrintStep.allCases) { Text($0.title).tag($0) }
            }
            .pickerStyle(.segmented).labelsHidden().controlSize(.regular)
            .accessibilityIdentifier("print-steps")

            switch store.step {
            case .orient: orientStep
            case .slice: sliceStep
            case .printer: printerStep
            case .print: printStep
            }

            if let e = store.error {
                Text(e).font(.caption).foregroundStyle(.red).textSelection(.enabled)
            }
            footer
        }
        .onAppear {
            store.loadPrinters()
            if store.poses.isEmpty { Task { await store.orient(model) } }
        }
        .onChange(of: store.step) { _, s in
            if s == .printer || s == .print { Task { await store.refreshPrinters() } }
        }
        .sheet(isPresented: $store.confirming) { confirmSheet }
        .sheet(isPresented: Binding(get: { pairing != nil }, set: { if !$0 { pairing = nil } })) {
            PairPrinterSheet(found: pairing ?? [:], store: store) { pairing = nil }
        }
    }

    // MARK: steps

    @ViewBuilder private var orientStep: some View {
        let groups = store.groups.filter { !$0.message.contains("overhang") }
        PrintGroup {
            if let p = store.selectedPose {
                if p.supportFree {
                    StatusLine(ok: true, title: "Prints with no supports",
                               detail: p.name == "As modeled" ? nil : "Rotated to rest on its flattest face.")
                } else {
                    StatusLine(ok: false, title: p.overhangCount == 1 ? "1 face overhangs the plate" : "\(p.overhangCount) faces overhang the plate",
                               detail: "Needs about \(fabFmt(p.supportVolume / 1000, 1)) cm³ of support.")
                }
            } else if store.busy {
                HStack { ProgressView().controlSize(.small); Text("Finding the best way up…").font(.callout).foregroundStyle(.secondary) }
            } else {
                Text("Open a part to print.").font(.callout).foregroundStyle(.secondary)
            }
            ForEach(groups) { g in StatusLine(ok: false, title: g.message) }
        }
        if !store.poses.isEmpty {
            PrintGroup("Orientation") {
                ForEach(store.poses) { p in
                    Button { store.pose = p.id; store.invalidateSlice() } label: {
                        HStack {
                            Image(systemName: store.pose == p.id ? "largecircle.fill.circle" : "circle")
                                .foregroundStyle(store.pose == p.id ? Color.accentColor : .secondary)
                            Text(p.name)
                            Spacer()
                            if p.fits {
                                Text(p.printTime.map(fabDuration) ?? "\(fabFmt(p.height, 0)) mm tall").monospacedDigit().foregroundStyle(.secondary)
                            } else {
                                Text("doesn't fit").foregroundStyle(.red)
                            }
                            Text(p.supportFree ? "no supports" : "supports").font(.caption)
                                .foregroundStyle(p.supportFree ? .green : .orange)
                        }
                        .font(.callout).contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
            Text("Ranked by support material, then print time.").font(.caption).foregroundStyle(.secondary)
        }
    }

    @ViewBuilder private var sliceStep: some View {
        PrintGroup {
            Picker("Printer profile", selection: $store.printer) {
                ForEach(store.printers, id: \.id) { Text($0.name).tag($0.id) }
                if store.printers.isEmpty { Text(store.printer).tag(store.printer) }
            }
            Picker("Material", selection: $store.material) {
                ForEach(["PLA", "PETG", "ABS", "TPU"], id: \.self) { Text($0).tag($0) }
            }
        }
        .onChange(of: store.printer) { store.invalidateSlice() }
        .onChange(of: store.material) { store.invalidateSlice() }
        PrintGroup("Quality") {
            VStack(alignment: .leading, spacing: 4) {
                Picker("Quality", selection: $store.quality) {
                    ForEach(PrintQuality.allCases) { Text($0.rawValue).tag($0) }
                }
                .pickerStyle(.segmented).labelsHidden()
                Text(store.quality.summary + " · supports " + (store.settings["support_enabled"] as? Bool == true ? "auto" : "off"))
                    .font(.caption).foregroundStyle(.secondary)
            }
            DisclosureGroup("Advanced") {
                VStack(alignment: .leading, spacing: 6) {
                    FabField(label: "Layer height", value: $store.layerHeight)
                    Stepper("Walls \(store.walls)", value: $store.walls, in: 1...10)
                    FabField(label: "Infill %", value: $store.infillPct, digits: 0)
                    Picker("Pattern", selection: $store.pattern) {
                        ForEach(["grid", "lines", "triangles", "honeycomb", "gyroid"], id: \.self) { Text($0.capitalized).tag($0) }
                    }
                    Toggle("Supports everywhere", isOn: $store.supports)
                    Stepper("Nozzle \(store.printTemp)°", value: $store.printTemp, in: 150...320, step: 5)
                    Stepper("Bed \(store.bedTemp)°", value: $store.bedTemp, in: 0...120, step: 5)
                }
                .padding(.top, 6)
                .onChange(of: store.settingsKey) { store.invalidateSlice() }
            }
            .font(.callout)
        }
        if let p = store.prepared { sliceResult(p) }
    }

    @ViewBuilder private func sliceResult(_ p: [String: Any]) -> some View {
        let stats = p["stats"] as? [String: Any] ?? [:]
        PrintGroup {
            HStack(alignment: .top) {
                bigNumber(fabDuration(fabNum(stats["print_time_seconds"])), "time")
                bigNumber("\(fabFmt(fabNum(stats["filament_grams"]))) g", "filament")
                bigNumber("\(p["layers"] as? Int ?? 0)", "layers")
            }
            if store.readyToPrint {
                StatusLine(ok: true, title: "Ready to print", detail: "Fits the plate · G-code lint clean · printcheck passed")
            } else {
                let msgs = (p["printcheck"] as? [[String: Any]] ?? []).compactMap { $0["message"] as? String }
                StatusLine(ok: false, title: "Not printable yet", detail: msgs.prefix(2).joined(separator: "; "))
            }
        }
        if let s = store.slice, let layers = s["layers"] as? [[String: Any]], !layers.isEmpty {
            let i = min(store.layer, layers.count - 1)
            let l = layers[i]
            FabRingsCanvas(closed: fabRings(l["outer"]), open: fabRings(l["infill"]), accent: fabRings(l["inner"]), height: 140)
            Slider(value: Binding(get: { Double(i) }, set: { store.layer = Int($0) }), in: 0...Double(max(1, layers.count - 1)))
            Text("Layer \((l["index"] as? Int ?? 0) + 1) · z \(fabFmt(fabNum(l["z"]), 2)) mm").font(.caption2.monospacedDigit()).foregroundStyle(.secondary)
        }
    }

    private func bigNumber(_ value: String, _ label: String) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(value).font(.title3.weight(.medium)).monospacedDigit()
            Text(label).font(.caption).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder private var printerStep: some View {
        if store.savedPrinters.isEmpty {
            PrintGroup {
                StatusLine(ok: true, title: "No printer yet",
                           detail: "vcad talks to Bambu printers directly on your network. No cloud account.", neutral: true)
            }
            PrintGroup("On this network") {
                if store.discovering {
                    HStack { ProgressView().controlSize(.small); Text("Searching…").foregroundStyle(.secondary) }.font(.callout)
                }
                ForEach(Array(store.discovered.enumerated()), id: \.offset) { _, p in
                    HStack {
                        Text("\(p["model"] as? String ?? "Printer") · \(p["name"] as? String ?? p["ip"] as? String ?? "")").lineLimit(1)
                        Spacer()
                        Button("Add…") { pairing = p }
                    }
                    .font(.callout)
                }
                Button("Enter IP address…") { pairing = [:] }.buttonStyle(.link).font(.callout)
            }
            .task { await store.discover() }
            Text("On the printer: Settings ▸ Network ▸ turn on LAN Only and Developer Mode. The access code is on the same screen.")
                .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
        } else {
            PrintGroup {
                Picker("Printer", selection: Binding(get: { store.device ?? "" }, set: { store.device = $0; Task { await store.refreshStatus() } })) {
                    ForEach(store.savedPrinters) { Text($0.name).tag($0.name) }
                }
                if let s = store.snapshot {
                    KeyValueRow("Status", s.stateLabel)
                    KeyValueRow("Nozzle", "\(Int(s.nozzle))°")
                    KeyValueRow("Bed", "\(Int(s.bed))°")
                } else {
                    Text("Not responding").font(.callout).foregroundStyle(.secondary)
                }
                Button("Add printer…") { pairing = [:] }.buttonStyle(.link).font(.callout)
            }
            if let s = store.snapshot, !s.trays.isEmpty {
                PrintGroup("AMS") {
                    ForEach(s.trays) { t in
                        Button { store.amsTray = t.tray } label: {
                            HStack(spacing: 8) {
                                Image(systemName: store.amsTray == t.tray ? "largecircle.fill.circle" : "circle")
                                    .foregroundStyle(store.amsTray == t.tray ? Color.accentColor : .secondary)
                                swatch(t.color)
                                Text(t.label)
                                Spacer()
                                Text(t.remaining.map { "\($0)%" } ?? "—").monospacedDigit().foregroundStyle(.secondary)
                            }
                            .font(.callout).contentShape(Rectangle())
                        }
                        .buttonStyle(.plain).disabled(t.type == nil)
                    }
                }
            }
            if let why = store.printBlocker {
                StatusLine(ok: false, title: why)
            } else {
                StatusLine(ok: true, title: "Job matches this printer",
                           detail: "\(store.material)\(store.amsTray.map { " in slot \($0 + 1)" } ?? "") · printer idle · model matches")
            }
        }
    }

    @ViewBuilder private var printStep: some View {
        if let s = store.snapshot, s.isActive || s.state == "finished" || store.sent != nil {
            let done = s.state == "finished"
            PrintGroup {
                VStack(alignment: .leading, spacing: 6) {
                    Text(done ? "Finished" : "\(s.stateLabel) on \(store.device ?? "printer")").font(.callout)
                    ProgressView(value: min(1, s.progress / 100)).progressViewStyle(.linear)
                    HStack(alignment: .top) {
                        bigNumber("\(Int(s.progress))%", "done")
                        bigNumber("\(s.remainingMin)m", "left")
                        bigNumber("\(s.layer)", "of \(s.layers) layers")
                    }
                }
                KeyValueRow("Nozzle / bed", "\(Int(s.nozzle))° / \(Int(s.bed))°")
                if !done { KeyValueRow("Finishes", Date().addingTimeInterval(Double(s.remainingMin) * 60).formatted(date: .omitted, time: .shortened)) }
            }
            Text(done ? "Let the plate cool before removing the part." : "Also under Jobs, so you can keep modeling. Closing this panel doesn't stop the print.")
                .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
        } else {
            PrintGroup { Text("Nothing printing. Slice a part and pick a printer first.").font(.callout).foregroundStyle(.secondary) }
        }
    }

    // MARK: footer

    private var footer: some View {
        HStack {
            if store.step != .print {
                Menu("Export") {
                    Button("Sliced .gcode.3mf…") { Task { await store.export(model, kind: "bambu") } }
                    Button("G-code…") { Task { await store.export(model, kind: "gcode") } }
                    Button("3MF project…") { Task { await store.export(model, kind: "3mf") } }
                }
                .fixedSize()
                .disabled(store.busy)
            } else if store.primary == .pause || store.primary == .resume {
                Button("Stop…", role: .destructive) { stopConfirm = true }
                    .confirmationDialog("Stop the print?", isPresented: $stopConfirm) {
                        Button("Stop print", role: .destructive) { Task { await store.control("stop") } }
                    } message: { Text("The part on the plate can't be resumed.") }
            }
            if let p = store.lastExport, store.step != .print {
                Image(systemName: "checkmark.circle").foregroundStyle(.green).help((p as NSString).lastPathComponent)
            }
            Spacer()
            primaryButton
        }
        .padding(.top, 4)
    }

    @State private var stopConfirm = false

    @ViewBuilder private var primaryButton: some View {
        switch store.primary {
        case .orientToFix:
            Button("Orient to fix") { store.fixByOrienting() }.buttonStyle(.borderedProminent)
        case .next:
            Button("Continue") { store.step = PrintStep(rawValue: store.step.rawValue + 1) ?? .print }
                .buttonStyle(.borderedProminent).keyboardShortcut(.defaultAction)
                .disabled(store.poses.isEmpty && store.step == .orient)
        case .slice:
            Button { Task { await store.sliceNow(model) } } label: {
                if store.busy { ProgressView().controlSize(.small) } else { Text("Slice") }
            }
            .buttonStyle(.borderedProminent).keyboardShortcut(.return, modifiers: [.command]).disabled(store.busy)
        case .print:
            Button("Print…") { store.confirming = true }
                .buttonStyle(.borderedProminent).disabled(store.printBlocker != nil || store.busy)
                .accessibilityIdentifier("print-send")
        case .pause:
            Button("Pause") { Task { await store.control("pause") } }.buttonStyle(.bordered)
        case .resume:
            Button("Resume") { Task { await store.control("resume"); store.startPolling() } }.buttonStyle(.borderedProminent)
        case .again:
            Button("Print another") { store.sent = nil; store.step = .orient }.buttonStyle(.borderedProminent)
        case .none:
            EmptyView()
        }
    }

    private var confirmSheet: some View {
        let stats = store.prepared?["stats"] as? [String: Any] ?? [:]
        return VStack(alignment: .leading, spacing: 12) {
            Text("Print on \(store.device ?? "printer")?").font(.headline)
            Text("\(fabDuration(fabNum(stats["print_time_seconds"]))) · \(fabFmt(fabNum(stats["filament_grams"]))) g of \(store.material)\(store.amsTray.map { " from AMS slot \($0 + 1)" } ?? ""). vcad uploads the file over your local network and starts the job. Make sure the plate is clear.")
                .font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { store.confirming = false }.keyboardShortcut(.cancelAction)
                Button("Print") {
                    store.confirming = false
                    Task { await store.startPrint(model) }
                }
                .keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent)
                .accessibilityIdentifier("print-confirm")
            }
        }
        .padding(20).frame(width: 340)
    }
}

extension PrintStore {
    /// Changes when any advanced setting changes (invalidates the slice).
    var settingsKey: String {
        "\(layerHeight)|\(walls)|\(infillPct)|\(pattern)|\(supports)|\(printTemp)|\(bedTemp)"
    }
}

/// Add a printer: discovered (IP/serial prefilled) or by hand.
private struct PairPrinterSheet: View {
    let found: [String: Any]
    @Bindable var store: PrintStore
    let done: () -> Void
    @State private var name = ""
    @State private var ip = ""
    @State private var serial = ""
    @State private var model = "P1S"
    @State private var code = ""
    @State private var saving = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(found.isEmpty ? "Add printer" : "Add \(found["model"] as? String ?? "printer")").font(.headline)
            Text("Enter the 8-character access code shown on the printer under Settings ▸ Network.")
                .font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            Form {
                TextField("Name", text: $name)
                if found.isEmpty {
                    TextField("IP address", text: $ip)
                    TextField("Serial number", text: $serial)
                    Picker("Model", selection: $model) {
                        ForEach(["P1S", "P1P", "X1C", "A1", "A1 mini"], id: \.self) { Text($0).tag($0) }
                    }
                }
                SecureField("Access code", text: $code)
            }
            if let e = store.error { Text(e).font(.caption).foregroundStyle(.red) }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { done() }.keyboardShortcut(.cancelAction)
                Button("Connect") {
                    saving = true
                    Task {
                        if await store.addPrinter(name: name, model: model, ip: ip, serial: serial, accessCode: code) { done() }
                        saving = false
                    }
                }
                .keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent)
                .disabled(saving || name.isEmpty || code.count != 8 || ip.isEmpty || serial.isEmpty)
            }
        }
        .padding(20).frame(width: 360)
        .onAppear {
            ip = found["ip"] as? String ?? ""
            serial = found["serial"] as? String ?? ""
            model = found["model"] as? String ?? "P1S"
            name = (found["name"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? model
        }
    }
}
