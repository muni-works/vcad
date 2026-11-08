import SwiftUI
import AppKit
import UniformTypeIdentifiers

typealias ECObject = [String: Any]
func ecRows(_ value: Any?) -> [ECObject] { value as? [ECObject] ?? [] }
func ecNumber(_ value: Any?, _ fallback: Double = 0) -> Double { (value as? NSNumber)?.doubleValue ?? fallback }
func ecPoint(_ value: Any?) -> CGPoint {
    let p = value as? ECObject ?? [:]
    return CGPoint(x: ecNumber(p["x"]), y: ecNumber(p["y"]))
}
func ecJSON(_ point: CGPoint) -> ECObject { ["x": point.x, "y": point.y] }

enum ElectronicsView: String, CaseIterable { case schematic = "Schematic", board = "Board", spatial = "3D" }
enum ElectronicsTool: String, CaseIterable { case select = "Select", place = "Place", wire = "Connect", route = "Route", via = "Via" }

@MainActor @Observable
final class ElectronicsWorkspace {
    var view: ElectronicsView = .board { didSet { routeStart = nil; pendingPin = nil; tool = .select } }
    var tool: ElectronicsTool = .select { didSet { routeStart = nil; pendingPin = nil } }
    var boardID: String?
    var selectedRef: String?
    var selectedTrace: Int?
    var selectedVia: Int?
    var componentKind = "Resistor"
    var activeLayer = "FCu"
    var activeNet = ""
    var traceWidth = 0.25
    var grid = 1.0
    var snap = true
    var zoom = 1.0
    var pan = CGSize.zero
    /// Cursor in board mm, for the status readout.
    var cursor: CGPoint?
    /// A part being dragged, and where it is right now.
    var dragRef: String?
    var dragPosition: CGPoint?
    /// Lower bound on zoom when centring on `focusPoint`.
    var focusMinZoom = 3.0
    /// Latest live route from `routeStart` to the cursor (router JSON).
    var routePreview: ECObject?
    var routeTarget: CGPoint?
    /// The cursor has snapped to copper of the routed net: a click finishes.
    var routeEndsOnCopper = false
    @ObservationIgnored private var router: PcbFFI.Router?
    @ObservationIgnored private var routerKey = -1
    @ObservationIgnored private var routeGeneration = 0
    @ObservationIgnored private var routeBusy = false
    @ObservationIgnored private var routeQueued: ECBox?
    private static let routeQueue = DispatchQueue(label: "vcad.interactive-route", qos: .userInitiated)
    /// The net lit on the canvas; everything else dims.
    var highlightNet: String?
    var hiddenLayers = Set<String>()
    var showRatsnest = true
    var showSilk = true
    var showCourtyard = false
    var showDrcMarkers = true
    /// A DRC location to centre and ring.
    var focusPoint: CGPoint?
    @ObservationIgnored private var airwireCache: (key: Int, wires: [Airwire]) = (-1, [])
    @ObservationIgnored private var ratsPendingKey = -1
    @ObservationIgnored private var ratsBusy = false
    @ObservationIgnored private var ratsQueued: (ECBox, Int)?
    private var ratsnestVersion = 0
    private static let ratsQueue = DispatchQueue(label: "vcad.ratsnest", qos: .userInitiated)
    private func boardKey(_ board: ECObject) -> Int {
        var h = Hasher()
        let t = ecRows(board["traces"]), v = ecRows(board["vias"])
        h.combine(t.count); h.combine(v.count)
        for x in t.suffix(8) { h.combine(ecPoint(x["end"]).x) }
        for x in ecRows(board["footprints"]) { let p = ecPoint(x["position"]); h.combine(p.x); h.combine(p.y); h.combine(ecNumber(x["rotation"])); h.combine(x["front"] as? Bool ?? true) }
        return h.finalize()
    }
    private func liveRouter(_ board: ECObject) -> PcbFFI.Router? {
        let key = boardKey(board)
        if router == nil || routerKey != key { router = try? PcbFFI.Router(board: board); routerKey = key }
        return router
    }
    /// Ask for a route to `target`. One search at a time, latest cursor wins.
    func updateRoute(_ model: EditorModel, to target: CGPoint, endsOnCopper: Bool) {
        guard let start = routeStart, tool == .route, !activeNet.isEmpty else { return }
        routeTarget = target; routeEndsOnCopper = endsOnCopper
        guard let router = liveRouter(board(model)) else { return }
        let req: ECObject = ["net": activeNet, "from": ecJSON(start), "to": ecJSON(target), "layer": activeLayer]
        routeGeneration += 1
        let generation = routeGeneration
        if routeBusy { routeQueued = ECBox(value: req); return }
        runRoute(router, req, generation)
    }
    private func runRoute(_ router: PcbFFI.Router, _ req: ECObject, _ generation: Int) {
        routeBusy = true
        let box = ECBox(value: req)
        Self.routeQueue.async { [weak self] in
            let result = (try? router.route(box.value)) ?? ["status": "blocked", "reason": "router error"]
            let out = ECBox(value: result)
            DispatchQueue.main.async {
                guard let self else { return }
                self.routeBusy = false
                if generation == self.routeGeneration, self.routeStart != nil { self.routePreview = out.value }
                if let next = self.routeQueued {
                    self.routeQueued = nil
                    self.runRoute(router, next.value, self.routeGeneration)
                }
            }
        }
    }
    func cancelRoute() {
        routeStart = nil; routePreview = nil; routeTarget = nil; routeGeneration += 1; routeQueued = nil
    }
    /// Commit the current preview. Finishes when it reached copper of the net;
    /// otherwise carries on from where it stopped.
    func commitRoute(_ model: EditorModel, at target: CGPoint, endsOnCopper: Bool) {
        // A click can outrun the cursor's preview; route to where it landed.
        if routePreview == nil || routeTarget != target, let start = routeStart, let r = liveRouter(board(model)) {
            routeGeneration += 1; routeQueued = nil
            routeTarget = target; routeEndsOnCopper = endsOnCopper
            routePreview = try? r.route(["net": activeNet, "from": ecJSON(start), "to": ecJSON(target), "layer": activeLayer])
        }
        guard let preview = routePreview, preview["status"] as? String == "routed",
              let path = preview["path"] as? [Any], let last = path.last as? [Any], last.count == 3,
              let router = liveRouter(board(model)) else {
            message = (routePreview?["reason"] as? String).map { "No legal path: \($0)" } ?? "No legal path to there."
            return
        }
        guard let added = try? router.commit(["net": activeNet, "path": path, "moved": preview["moved"] ?? [Any]()]) else {
            message = "That path is no longer legal."; return
        }
        let end = ecPoint(last[1]), finished = routeEndsOnCopper
        mutateBoard(model) { b in
            var existing = ecRows(b["traces"])
            for m in ecRows(added["moved"]) {
                let i = Int(ecNumber(m["index"], -1))
                guard existing.indices.contains(i) else { continue }
                existing[i]["start"] = m["start"]; existing[i]["end"] = m["end"]
            }
            b["traces"] = existing + ecRows(added["traces"])
            b["vias"] = ecRows(b["vias"]) + ecRows(added["vias"])
        }
        // The session already holds this copper; keep it rather than re-index.
        routerKey = boardKey(board(model))
        if let l = last[2] as? String { activeLayer = l }
        routePreview = nil
        if finished { cancelRoute() } else { routeStart = end }
    }
    func nextRouteLayer() {
        let layers = BoardLayers.copper
        activeLayer = layers[((layers.firstIndex(of: activeLayer) ?? 0) + 1) % layers.count]
    }
    /// The board as drawn: the committed one, with any part mid-drag moved.
    func drawBoard(_ model: EditorModel) -> ECObject {
        var b = board(model)
        guard let ref = dragRef, let p = dragPosition else { return b }
        var rows = ecRows(b["footprints"])
        if let i = rows.firstIndex(where: { $0["ref"] as? String == ref }) {
            rows[i]["position"] = ecJSON(p); b["footprints"] = rows
        }
        return b
    }
    func focus(on p: CGPoint, minZoom: Double = 3) {
        focusMinZoom = minZoom
        focusPoint = nil
        focusPoint = p
    }
    /// Unrouted gaps per net, worst first.
    func openNets(_ board: ECObject) -> [(name: String, gaps: Int, at: CGPoint)] {
        var out: [(name: String, gaps: Int, at: CGPoint)] = []
        for (net, wires) in Dictionary(grouping: airwires(board), by: { $0.net }) {
            let w = wires[0]
            out.append((net, wires.count, CGPoint(x: (w.a.x + w.b.x) / 2, y: (w.a.y + w.b.y) / 2)))
        }
        return out.sorted { $0.gaps != $1.gaps ? $0.gaps > $1.gaps : $0.name < $1.name }
    }
    /// Move the selected part to the other side of the board, mirrored about
    /// its own vertical axis — copper, mask, paste and artwork together.
    func flipSelected(_ model: EditorModel) {
        guard let ref = selectedRef else { return }
        func other(_ l: String) -> String {
            if l.hasPrefix("F") { return "B" + l.dropFirst() }
            if l.hasPrefix("B") { return "F" + l.dropFirst() }
            return l
        }
        func mirrored(_ value: Any?) -> ECObject { let p = ecPoint(value); return ecJSON(CGPoint(x: -p.x, y: p.y)) }
        mutateBoard(model) { b in
            var rows = ecRows(b["footprints"])
            guard let i = rows.firstIndex(where: { $0["ref"] as? String == ref }) else { return }
            var f = rows[i]
            f["front"] = !(f["front"] as? Bool ?? true)
            f["rotation"] = 0 - ecNumber(f["rotation"])
            f["pads"] = ecRows(f["pads"]).map { pad -> ECObject in
                var pad = pad
                pad["position"] = mirrored(pad["position"])
                pad["rotation"] = 0 - ecNumber(pad["rotation"])
                pad["layers"] = (pad["layers"] as? [String] ?? []).map(other)
                return pad
            }
            f["graphics"] = ecRows(f["graphics"]).map { g -> ECObject in
                var g = g
                g["layer"] = other(g["layer"] as? String ?? "")
                for key in ["start", "end", "center", "position"] where g[key] != nil { g[key] = mirrored(g[key]) }
                if g["vertices"] != nil { g["vertices"] = ecRows(g["vertices"]).map { mirrored($0) } }
                return g
            }
            rows[i] = f
            b["footprints"] = rows
        }
    }
    /// The airwires for `board`, from the kernel off the main thread: the
    /// last answer until the new one lands, latest edit wins.
    func airwires(_ board: ECObject) -> [Airwire] {
        _ = ratsnestVersion
        let key = boardKey(board)
        if key != airwireCache.key { requestRatsnest(board, key) }
        return airwireCache.wires
    }
    private func requestRatsnest(_ board: ECObject, _ key: Int) {
        guard key != ratsPendingKey else { return }
        ratsPendingKey = key
        // Only pads and copper matter; footprint art and source are most of the JSON.
        var light = board
        light["footprints"] = ecRows(board["footprints"]).map { f -> ECObject in
            var f = f; f["graphics"] = [Any](); f["properties"] = ECObject(); return f
        }
        ratsQueued = (ECBox(value: light), key)
        guard !ratsBusy else { return }
        runRatsnest()
    }
    private func runRatsnest() {
        guard let (box, key) = ratsQueued else { ratsBusy = false; return }
        ratsQueued = nil; ratsBusy = true
        Self.ratsQueue.async { [weak self] in
            let result = ECBox(value: (try? PcbFFI.ratsnest(box.value)) ?? [:])
            DispatchQueue.main.async {
                guard let self else { return }
                let wires = ecRows(result.value["wires"]).map {
                    Airwire(a: ecPoint($0["from"]), b: ecPoint($0["to"]), net: $0["net"] as? String ?? "")
                }
                self.airwireCache = (key, wires)
                self.ratsnestVersion += 1
                self.runRatsnest()
            }
        }
    }
    var routeStart: CGPoint?
    var pendingPin: String?
    var message: String?
    var issues: [String] = []
    var checking = false
    let kinds = ["Resistor", "Capacitor", "Voltage source", "Connector"]
    /// Kernel-backed tools (DRC, routing, fab, enclosure, simulation).
    let tools = ElectronicsTools()

    func boards(_ model: EditorModel) -> [String] {
        _ = model.undoStack.count; _ = model.redoStack.count; _ = model.documentDirty
        let nodes = model.documentJSON?["nodes"] as? ECObject ?? [:]
        return nodes.keys.filter { ((nodes[$0] as? ECObject)?["op"] as? ECObject)?["type"] as? String == "PcbBoard" }.sorted()
    }
    func board(_ model: EditorModel) -> ECObject {
        let ids = boards(model)
        let id = ids.contains(boardID ?? "") ? boardID : ids.first
        if let id, let node = (model.documentJSON?["nodes"] as? ECObject)?[id] as? ECObject,
           let op = node["op"] as? ECObject, let board = op["board"] as? ECObject { return board }
        return model.documentJSON?["pcb"] as? ECObject ?? [:]
    }
    func schematic(_ model: EditorModel) -> ECObject {
        _ = model.undoStack.count; _ = model.redoStack.count
        return model.documentJSON?["schematic"] as? ECObject ?? [:]
    }
    func mutateBoard(_ model: EditorModel, _ mutate: (inout ECObject) -> Void) {
        let ids = boards(model), chosen = ids.contains(boardID ?? "") ? boardID : ids.first
        model.editElectronics { doc in
            if let chosen {
                var nodes = doc["nodes"] as? ECObject ?? [:], node = nodes[chosen] as? ECObject ?? [:]
                var op = node["op"] as? ECObject ?? [:], b = op["board"] as? ECObject ?? [:]
                mutate(&b); op["board"] = b; node["op"] = op; nodes[chosen] = node; doc["nodes"] = nodes
            } else {
                var b = doc["pcb"] as? ECObject ?? [:]; mutate(&b); doc["pcb"] = b
            }
        }
        issues = []; message = nil
    }
    func createBoard(_ model: EditorModel) {
        guard board(model).isEmpty else { return }
        guard model.source.isSandbox || model.documentJSON != nil else { message = "Open an editable vcad document before adding electronics."; return }
        model.editElectronics { doc in
            var nodes = doc["nodes"] as? ECObject ?? [:]
            let id = String((nodes.keys.compactMap(Int.init).max() ?? -1) + 1)
            nodes[id] = ["id": Int(id)!, "name": "Circuit board", "op": ["type": "PcbBoard", "board": Self.emptyBoard]]
            doc["nodes"] = nodes
            var roots = ecRows(doc["roots"]); roots.append(["root": Int(id)!, "material": "default"]); doc["roots"] = roots
            if doc["schematic"] == nil { doc["schematic"] = ["components": [ECObject](), "wires": [ECObject](), "junctions": [ECObject](), "labels": [ECObject](), "nets": [String: [String]]()] }
            boardID = id
        }
    }
    static var emptyBoard: ECObject {
        ["outline": ["vertices": [["x": 0, "y": 0], ["x": 80, "y": 0], ["x": 80, "y": 50], ["x": 0, "y": 50]], "thickness": 1.6],
         "stackup": ["layers": [["layer": "FCu", "copperThickness": 0.035, "dielectricThickness": 1.53], ["layer": "BCu", "copperThickness": 0.035]]],
         "nets": [ECObject](), "footprints": [ECObject](), "traces": [ECObject](), "vias": [ECObject](), "zones": [ECObject](),
         "rules": ["defaultRules": ["name": "Default", "traceWidth": 0.25, "clearance": 0.2, "viaDiameter": 0.6, "viaDrill": 0.3], "edgeClearance": 0.5, "holeToHole": 0.25, "minAnnularRing": 0.15, "minDrill": 0.3]]
    }
    func snapped(_ p: CGPoint) -> CGPoint {
        guard snap, grid.isFinite, grid > 0 else { return p }
        return CGPoint(x: (p.x / grid).rounded() * grid, y: (p.y / grid).rounded() * grid)
    }
    func place(_ model: EditorModel, at point: CGPoint) {
        let prefix = componentKind == "Resistor" ? "R" : componentKind == "Capacitor" ? "C" : componentKind == "Voltage source" ? "V" : "J"
        let existing = ecRows(board(model)["footprints"]).compactMap { $0["ref"] as? String } + ecRows(schematic(model)["components"]).compactMap { $0["ref"] as? String }
        var index = 1; while existing.contains("\(prefix)\(index)") { index += 1 }
        let undoBefore = model.undoStack
        let before = model.documentJSON.flatMap { DocEdit.serialize($0) }
        let ref = "\(prefix)\(index)", point = snapped(point)
        let pads: [ECObject] = [-2.0, 2.0].enumerated().map { i, x in
            ["number": "\(i + 1)", "padType": "SMD", "shape": ["type": "Rect", "width": 1.5, "height": 2.0], "position": ["x": x, "y": 0], "layers": ["FCu"]]
        }
        let value = componentKind == "Resistor" ? "10k" : componentKind == "Capacitor" ? "100nF" : componentKind == "Voltage source" ? "5" : "2-pin"
        let fp: ECObject = ["ref": ref, "value": value, "footprintName": "vcad:Native_2Pad", "position": ecJSON(point), "rotation": 0, "front": true, "pads": pads, "graphics": [ECObject]()]
        mutateBoard(model) { b in var f = ecRows(b["footprints"]); f.append(fp); b["footprints"] = f }
        // Add schematic and board as one undoable placement.
        model.editElectronics { doc in
            var sch = doc["schematic"] as? ECObject ?? [:], components = ecRows(sch["components"])
            let pins: [ECObject] = [-2.0, 2.0].enumerated().map { i, x in ["number": "\(i + 1)", "name": "\(i + 1)", "pin_type": "Passive", "position": ["x": x, "y": 0]] }
            components.append(["ref": ref, "value": value, "footprintId": "vcad:Native_2Pad", "position": ecJSON(point), "pins": pins, "pads": pads])
            sch["components"] = components; sch["wires"] = sch["wires"] ?? [ECObject](); sch["junctions"] = sch["junctions"] ?? [ECObject](); sch["labels"] = sch["labels"] ?? [ECObject](); doc["schematic"] = sch
        }
        if let before { model.undoStack = Array((undoBefore + [UndoEntry(data: before, name: "Place Component")]).suffix(64)) }
        selectedRef = ref   // stay in Place until Esc, like every EDA tool (it reverted to Select after one part)
    }
    func editComponent(_ model: EditorModel, key: String, value: Any) {
        guard let ref = selectedRef else { return }
        if key == "value" {
            let undoBefore = model.undoStack
            let before = model.documentJSON.flatMap { DocEdit.serialize($0) }
            mutateBoard(model) { b in
                var rows = ecRows(b["footprints"])
                if let i = rows.firstIndex(where: { $0["ref"] as? String == ref }) { rows[i][key] = value }
                b["footprints"] = rows
            }
            model.editElectronics { doc in
                var sch = doc["schematic"] as? ECObject ?? [:], rows = ecRows(sch["components"])
                if let i = rows.firstIndex(where: { $0["ref"] as? String == ref }) { rows[i][key] = value }
                sch["components"] = rows; doc["schematic"] = sch
            }
            if let before { model.undoStack = Array((undoBefore + [UndoEntry(data: before, name: "Change Value")]).suffix(64)) }
            return
        }
        if view == .schematic {
            model.editElectronics { doc in
                var sch = doc["schematic"] as? ECObject ?? [:], rows = ecRows(sch["components"])
                if let i = rows.firstIndex(where: { $0["ref"] as? String == ref }) { rows[i][key] = value }
                sch["components"] = rows; doc["schematic"] = sch
            }
        } else {
            mutateBoard(model) { b in var rows = ecRows(b["footprints"]); if let i = rows.firstIndex(where: { $0["ref"] as? String == ref }) { rows[i][key] = value }; b["footprints"] = rows }
        }
    }
    func addTrace(_ model: EditorModel, to point: CGPoint, snapToGrid: Bool = true) {
        guard !activeNet.isEmpty, ecRows(board(model)["nets"]).contains(where: { $0["id"] as? String == activeNet }), (0.1...5).contains(traceWidth) else { message = "Choose a net and a trace width between 0.1 and 5 mm."; return }
        let end = snapToGrid ? snapped(point) : point
        guard let start = routeStart else { routeStart = end; return }
        guard start != end else { return }
        mutateBoard(model) { b in var traces = ecRows(b["traces"]); traces.append(["start": ecJSON(start), "end": ecJSON(end), "width": traceWidth, "layer": activeLayer, "net": activeNet, "source": "manual"]); b["traces"] = traces }
        routeStart = end
    }
    func addVia(_ model: EditorModel, at p: CGPoint) {
        guard !activeNet.isEmpty else { message = "Choose a net before placing a via."; return }
        mutateBoard(model) { b in var vias = ecRows(b["vias"]); vias.append(["position": ecJSON(snapped(p)), "diameter": 0.6, "drill": 0.3, "startLayer": "FCu", "endLayer": "BCu", "net": activeNet, "source": "manual"]); b["vias"] = vias }
    }
    func connect(_ model: EditorModel, pin: String) {
        guard let start = pendingPin else { pendingPin = pin; return }
        guard start != pin else { return }
        let undoBefore = model.undoStack
        let before = model.documentJSON.flatMap { DocEdit.serialize($0) }
        var nets = schematic(model)["nets"] as? [String: [String]] ?? [:]
        let matching = nets.keys.filter { nets[$0]!.contains(start) || nets[$0]!.contains(pin) }
        var next = 1
        while nets["N\(next)"] != nil { next += 1 }
        let name = matching.sorted().first ?? "N\(next)"
        var refs = Set([start, pin]); for key in matching { refs.formUnion(nets.removeValue(forKey: key) ?? []) }; nets[name] = refs.sorted()
        model.editElectronics { doc in var sch = doc["schematic"] as? ECObject ?? [:]; sch["nets"] = nets; doc["schematic"] = sch }
        // Net assignment updates are committed with the schematic by syncBoardNets.
        syncBoardNets(model, nets: nets, merged: matching, target: name)
        if let before { model.undoStack = Array((undoBefore + [UndoEntry(data: before, name: "Connect Pins")]).suffix(64)) }
        pendingPin = nil; activeNet = name
    }
    private func syncBoardNets(_ model: EditorModel, nets: [String: [String]], merged: [String], target: String) {
        mutateBoard(model) { b in
            var rows = ecRows(b["nets"]).filter { !merged.contains($0["id"] as? String ?? "") && $0["id"] as? String != target }
            rows.append(["id": target, "name": target]); b["nets"] = rows
            var footprints = ecRows(b["footprints"])
            for i in footprints.indices {
                var pads = ecRows(footprints[i]["pads"])
                for j in pads.indices {
                    let ref = "\(footprints[i]["ref"] as? String ?? "").\(pads[j]["number"] as? String ?? "")"
                    if let net = nets.first(where: { $0.value.contains(ref) }) { pads[j]["net"] = net.key }
                }
                footprints[i]["pads"] = pads
            }
            b["footprints"] = footprints
            for key in ["traces", "vias", "traceArcs", "zones"] {
                if b[key] != nil { b[key] = ecRows(b[key]).map { row in var row = row; if merged.contains(row["net"] as? String ?? "") { row["net"] = target }; return row } }
            }
        }
    }
    func removeSelection(_ model: EditorModel) {
        if let i = selectedTrace {
            mutateBoard(model) { b in var rows = ecRows(b["traces"]); if rows.indices.contains(i) { rows.remove(at: i) }; b["traces"] = rows }; selectedTrace = nil
        } else if let i = selectedVia {
            mutateBoard(model) { b in var rows = ecRows(b["vias"]); if rows.indices.contains(i) { rows.remove(at: i) }; b["vias"] = rows }; selectedVia = nil
        }
    }
    func check(_ model: EditorModel) {
        issues = []
        let b = board(model), footprints = ecRows(b["footprints"]), nets = ecRows(b["nets"]).compactMap { $0["id"] as? String }
        var seen = Set<String>()
        for f in footprints {
            let ref = f["ref"] as? String ?? "?"
            if !seen.insert(ref).inserted { issues.append("Duplicate reference \(ref)") }
            for pad in ecRows(f["pads"]) where (pad["net"] as? String ?? "").isEmpty { issues.append("\(ref).\(pad["number"] as? String ?? "?") has no net") }
        }
        for (i, trace) in ecRows(b["traces"]).enumerated() {
            if !nets.contains(trace["net"] as? String ?? "") { issues.append("Trace \(i + 1) references an unknown net") }
            if ecNumber(trace["width"]) < 0.1 { issues.append("Trace \(i + 1) is narrower than 0.1 mm") }
        }
        checking = true
    }
}
