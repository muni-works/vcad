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
enum ElectronicsTool: String, CaseIterable { case select = "Select", place = "Place", wire = "Connect", label = "Label", zone = "Zone", route = "Route", via = "Via" }

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
    var symbolItems: [String] = []
    var symbolSearch = ""
    var symbolChoice: String?
    /// Open child sheet by name; nil is the root.
    var activeSheet: String?
    /// Corners of the zone being drawn.
    var zonePoints: [CGPoint] = []
    var selectedZone: Int?
    /// Poured copper per zone, as the fab will see it.
    var zoneFills: [ECObject] = []
    var teardrops: [ECObject] = []
    /// Parts selected together (shift-click), for align and distribute.
    var multiRefs = Set<String>()
    /// KiCad library footprints as "Library:Name".
    var libraryItems: [String] = []
    var librarySearch = ""
    var libraryChoice: String?
    var selectedWire: Int?
    var selectedLabel: Int?
    /// Corners of the schematic wire being drawn.
    var wirePoints: [CGPoint] = []
    /// Text the Net Label tool places.
    var labelText = ""
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
        for z in ecRows(board["zones"]) {
            h.combine(z["net"] as? String); h.combine(z["layer"] as? String)
            for p in ecRows(z["outline"]) { h.combine(ecPoint(p).x); h.combine(ecPoint(p).y) }
        }
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
            var out = (try? PcbFFI.ratsnest(box.value)) ?? [:]
            if let fill = try? PcbFFI.fillZones(box.value) { out["zones"] = fill["zones"]; out["teardrops"] = fill["teardrops"] }
            let result = ECBox(value: out)
            DispatchQueue.main.async {
                guard let self else { return }
                let wires = ecRows(result.value["wires"]).map {
                    Airwire(a: ecPoint($0["from"]), b: ecPoint($0["to"]), net: $0["net"] as? String ?? "")
                }
                self.airwireCache = (key, wires)
                if let zones = result.value["zones"] { self.zoneFills = ecRows(zones) }
                self.teardrops = ecRows(result.value["teardrops"])
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
        return model.documentJSON.map { sheetData($0) } ?? [:]
    }
    /// The whole design's root sheet, whichever sheet is open.
    func rootSchematic(_ model: EditorModel) -> ECObject {
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
            var sch = sheetData(doc), components = ecRows(sch["components"])
            let pins: [ECObject] = [-2.0, 2.0].enumerated().map { i, x in ["number": "\(i + 1)", "name": "\(i + 1)", "pin_type": "Passive", "position": ["x": x, "y": 0]] }
            components.append(["ref": ref, "value": value, "footprintId": "vcad:Native_2Pad", "position": ecJSON(point), "pins": pins, "pads": pads])
            sch["components"] = components; sch["wires"] = sch["wires"] ?? [ECObject](); sch["junctions"] = sch["junctions"] ?? [ECObject](); sch["labels"] = sch["labels"] ?? [ECObject](); setSheet(&doc, sch)
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
                var sch = sheetData(doc), rows = ecRows(sch["components"])
                if let i = rows.firstIndex(where: { $0["ref"] as? String == ref }) { rows[i][key] = value }
                sch["components"] = rows; setSheet(&doc, sch)
            }
            if let before { model.undoStack = Array((undoBefore + [UndoEntry(data: before, name: "Change Value")]).suffix(64)) }
            return
        }
        if view == .schematic {
            model.editElectronics { doc in
                var sch = sheetData(doc), rows = ecRows(sch["components"])
                if let i = rows.firstIndex(where: { $0["ref"] as? String == ref }) { rows[i][key] = value }
                sch["components"] = rows; setSheet(&doc, sch)
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
        model.editElectronics { doc in var sch = sheetData(doc); sch["nets"] = nets; setSheet(&doc, sch) }
        // Net assignment updates are committed with the schematic by syncBoardNets.
        syncBoardNets(model, nets: nets, merged: matching, target: name)
        if let before { model.undoStack = Array((undoBefore + [UndoEntry(data: before, name: "Connect Pins")]).suffix(64)) }
        pendingPin = nil; activeNet = name
    }
    func syncBoardNets(_ model: EditorModel, nets: [String: [String]], merged: [String], target: String) {
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


extension ElectronicsWorkspace {
    // MARK: zones

    func zoneClick(_ model: EditorModel, at p: CGPoint, tolerance: CGFloat) {
        if zonePoints.count >= 3, hypot(zonePoints[0].x - p.x, zonePoints[0].y - p.y) < tolerance { finishZone(model); return }
        zonePoints.append(snapped(p))
    }
    func finishZone(_ model: EditorModel) {
        guard zonePoints.count >= 3 else { zonePoints = []; return }
        guard !activeNet.isEmpty else { message = "Choose a net for the zone first."; return }
        let zone: ECObject = ["outline": zonePoints.map(ecJSON), "holes": [Any](), "net": activeNet, "layer": activeLayer,
                              "clearance": 0.2, "minArea": 0.5, "fillType": "Solid", "thermalRelief": "Relief",
                              "thermalGap": 0.3, "thermalSpokeWidth": 0.4, "priority": 0]
        zonePoints = []
        mutateBoard(model) { b in b["zones"] = ecRows(b["zones"]) + [zone] }
    }
    func selectZone(_ model: EditorModel, at p: CGPoint) {
        for (i, z) in ecRows(board(model)["zones"]).enumerated().reversed() where !hiddenLayers.contains(z["layer"] as? String ?? "") {
            let ring = ecRows(z["outline"]).map { ecPoint($0) }
            var inside = false, j = ring.count - 1
            for k in ring.indices {
                if (ring[k].y > p.y) != (ring[j].y > p.y), p.x < (ring[j].x - ring[k].x) * (p.y - ring[k].y) / (ring[j].y - ring[k].y) + ring[k].x { inside.toggle() }
                j = k
            }
            if inside { selectedZone = i; highlightNet = z["net"] as? String; return }
        }
    }
    func deleteSelectedZone(_ model: EditorModel) {
        guard let i = selectedZone else { return }
        mutateBoard(model) { b in var rows = ecRows(b["zones"]); if rows.indices.contains(i) { rows.remove(at: i) }; b["zones"] = rows }
        selectedZone = nil
    }

    // MARK: align

    enum Align { case left, right, top, bottom, spreadX, spreadY }
    func align(_ model: EditorModel, _ how: Align) {
        let onSheet = view == .schematic
        func apply(_ rows: [ECObject]) -> [ECObject] {
            let picked = rows.indices.filter { multiRefs.contains(rows[$0]["ref"] as? String ?? "") }
            let pts = picked.map { ecPoint(rows[$0]["position"]) }
            guard let x0 = pts.map(\.x).min(), let x1 = pts.map(\.x).max(), let y0 = pts.map(\.y).min(), let y1 = pts.map(\.y).max() else { return rows }
            var out = rows
            let order = picked.sorted { how == .spreadX ? ecPoint(rows[$0]["position"]).x < ecPoint(rows[$1]["position"]).x : ecPoint(rows[$0]["position"]).y < ecPoint(rows[$1]["position"]).y }
            for (n, i) in order.enumerated() {
                var p = ecPoint(rows[i]["position"])
                let t = CGFloat(n) / CGFloat(max(1, order.count - 1))
                switch how {
                case .left: p.x = x0
                case .right: p.x = x1
                case .top: p.y = y0
                case .bottom: p.y = y1
                case .spreadX: p.x = x0 + (x1 - x0) * t
                case .spreadY: p.y = y0 + (y1 - y0) * t
                }
                out[i]["position"] = ecJSON(p)
            }
            return out
        }
        if onSheet {
            model.editElectronics(name: "Align") { doc in
                var s = sheetData(doc); s["components"] = apply(ecRows(s["components"])); setSheet(&doc, s)
            }
        } else {
            mutateBoard(model) { b in b["footprints"] = apply(ecRows(b["footprints"])) }
        }
    }

    // MARK: footprint library

    /// Index the KiCad footprint folders once, off the main thread.
    func loadLibrary() {
        guard libraryItems.isEmpty else { return }
        Task {
            let items = await Task.detached { () -> [String] in
                var out: [String] = []
                let fm = FileManager.default
                let roots = [ElectronicsWorkspace.userFootprintRoot, "/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints", "/usr/share/kicad/footprints", "/usr/local/share/kicad/footprints"]
                for root in roots {
                    for lib in (try? fm.contentsOfDirectory(atPath: root)) ?? [] where lib.hasSuffix(".pretty") {
                        let name = String(lib.dropLast(7))
                        for f in (try? fm.contentsOfDirectory(atPath: root + "/" + lib)) ?? [] where f.hasSuffix(".kicad_mod") {
                            out.append("\(name):\(f.dropLast(10))")
                        }
                    }
                }
                return out.sorted()
            }.value
            libraryItems = items
        }
    }
    func libraryMatches() -> [String] {
        let terms = librarySearch.lowercased().split(separator: " ")
        guard !terms.isEmpty else { return [] }
        return Array(libraryItems.lazy.filter { id in let l = id.lowercased(); return terms.allSatisfy { l.contains($0) } }.prefix(60))
    }
    /// Place the chosen library footprint, with a matching schematic symbol.
    func placeLibrary(_ model: EditorModel, id: String, at point: CGPoint) {
        let lib = id.split(separator: ":").first.map(String.init) ?? ""
        let prefix: String
        switch true {
        case lib.hasPrefix("Resistor"): prefix = "R"
        case lib.hasPrefix("Capacitor"): prefix = "C"
        case lib.hasPrefix("Inductor"): prefix = "L"
        case lib.hasPrefix("Diode"), lib.hasPrefix("LED"): prefix = "D"
        case lib.hasPrefix("Connector"): prefix = "J"
        case lib.hasPrefix("Crystal"): prefix = "Y"
        default: prefix = "U"
        }
        let existing = ecRows(board(model)["footprints"]).compactMap { $0["ref"] as? String } + ecRows(schematic(model)["components"]).compactMap { $0["ref"] as? String }
        var n = 1; while existing.contains("\(prefix)\(n)") { n += 1 }
        let ref = "\(prefix)\(n)", at = snapped(point)
        let copper = ecRows(board(model)["stackup"].flatMap { ($0 as? ECObject)?["layers"] }).compactMap { $0["layer"] as? String }
        setenv("VCAD_KICAD_FOOTPRINTS", Self.userFootprintRoot, 1)
        guard let r = try? PcbFFI.libraryFootprint(["id": id, "ref": ref, "position": ecJSON(at), "copper": copper.isEmpty ? ["FCu", "BCu"] : copper]),
              var fp = r["footprint"] as? ECObject else { message = "Couldn't load \(id)."; return }
        fp["value"] = String(id.split(separator: ":").last ?? "")
        let numbers = Array(Set(ecRows(fp["pads"]).compactMap { $0["number"] as? String }.filter { !$0.isEmpty })).sorted { $0.localizedStandardCompare($1) == .orderedAscending }
        let pins: [ECObject] = numbers.map { ["number": $0, "name": $0, "pin_type": "Passive", "position": ["x": 0, "y": 0]] }
        let undoBefore = model.undoStack
        let before = model.documentJSON.flatMap { DocEdit.serialize($0) }
        mutateBoard(model) { b in b["footprints"] = ecRows(b["footprints"]) + [fp] }
        model.editElectronics(name: "Place") { doc in
            var s = sheetData(doc)
            let c: ECObject = ["ref": ref, "value": fp["value"] ?? "", "footprintId": id, "position": ecJSON(at), "rotation": 0, "mirror": false, "pins": pins]
            s["components"] = ecRows(s["components"]) + [c]; setSheet(&doc, s)
        }
        if let before { model.undoStack = Array((undoBefore + [UndoEntry(data: before, name: "Place \(ref)")]).suffix(64)) }
        selectedRef = ref
    }
}


extension ElectronicsWorkspace {
    // MARK: hierarchical sheets

    func sheetData(_ doc: ECObject) -> ECObject {
        let root = doc["schematic"] as? ECObject ?? [:]
        guard let name = activeSheet, let h = root["hierarchy"] as? ECObject else { return root }
        return ecRows(h["sheets"]).first { $0["name"] as? String == name }?["body"] as? ECObject ?? root
    }
    func setSheet(_ doc: inout ECObject, _ value: ECObject) {
        guard let name = activeSheet, var root = doc["schematic"] as? ECObject, var h = root["hierarchy"] as? ECObject else {
            doc["schematic"] = value; return
        }
        h["sheets"] = ecRows(h["sheets"]).map { d -> ECObject in
            var d = d; if d["name"] as? String == name { d["body"] = value }; return d
        }
        root["hierarchy"] = h; doc["schematic"] = root
    }
    func sheetNames(_ model: EditorModel) -> [String] {
        ecRows((rootSchematic(model)["hierarchy"] as? ECObject)?["sheets"]).compactMap { $0["name"] as? String }
    }
    /// Move the selected symbols to a new child sheet. Nets they share with
    /// what stays become global, so the design's connectivity is unchanged.
    func moveSelectionToNewSheet(_ model: EditorModel, named name: String) {
        var picked = multiRefs
        if let r = selectedRef { picked.insert(r) }
        let clean = name.trimmingCharacters(in: .whitespaces)
        guard !picked.isEmpty, !clean.isEmpty, !sheetNames(model).contains(clean), activeSheet == nil else {
            message = "Select parts on the root sheet and give the new sheet a unique name."; return
        }
        model.editElectronics(name: "Move to Sheet") { doc in
            var root = doc["schematic"] as? ECObject ?? [:]
            let all = ecRows(root["components"])
            let moving = all.filter { picked.contains($0["ref"] as? String ?? "") }
            let nets = root["nets"] as? [String: [String]] ?? [:]
            func inSet(_ pin: String) -> Bool { picked.contains(String(pin.split(separator: ".")[0])) }
            var childNets: [String: [String]] = [:], rootNets: [String: [String]] = [:], shared: [String] = []
            for (net, pins) in nets {
                let a = pins.filter(inSet), b = pins.filter { !inSet($0) }
                if !a.isEmpty { childNets[net] = a }
                if !b.isEmpty { rootNets[net] = b }
                if !a.isEmpty, !b.isEmpty { shared.append(net) }
            }
            let body: ECObject = ["title": clean, "components": moving, "wires": [ECObject](), "junctions": [ECObject](), "labels": [ECObject](), "nets": childNets]
            var h = root["hierarchy"] as? ECObject ?? ["sheets": [ECObject](), "instances": [ECObject](), "global_nets": [String]()]
            h["sheets"] = ecRows(h["sheets"]) + [["name": clean, "ports": [ECObject](), "body": body] as ECObject]
            h["instances"] = ecRows(h["instances"]) + [["name": clean, "sheet": clean, "connections": [String: String]()] as ECObject]
            h["global_nets"] = Array(Set((h["global_nets"] as? [String] ?? []) + shared)).sorted()
            root["components"] = all.filter { !picked.contains($0["ref"] as? String ?? "") }
            root["nets"] = rootNets; root["hierarchy"] = h
            doc["schematic"] = root
        }
        multiRefs = []; selectedRef = nil
    }

    // MARK: footprint and symbol editing

    func editFootprint(_ model: EditorModel, _ change: (inout ECObject) -> Void) {
        guard let ref = selectedRef else { return }
        mutateBoard(model) { b in
            var rows = ecRows(b["footprints"])
            guard let i = rows.firstIndex(where: { $0["ref"] as? String == ref }) else { return }
            change(&rows[i]); b["footprints"] = rows
        }
    }
    func editSymbol(_ model: EditorModel, _ change: (inout ECObject) -> Void) {
        guard let ref = selectedRef else { return }
        model.editElectronics(name: "Edit Symbol") { doc in
            var s = sheetData(doc), rows = ecRows(s["components"])
            guard let i = rows.firstIndex(where: { $0["ref"] as? String == ref }) else { return }
            change(&rows[i]); s["components"] = rows; setSheet(&doc, s)
        }
    }

    // MARK: pairs and length

    /// The other half of a differential pair, by the usual naming.
    func pairPartner(of net: String, in board: ECObject) -> String? {
        let ids = Set(ecRows(board["nets"]).compactMap { $0["id"] as? String })
        for (a, b) in [("_P", "_N"), ("_N", "_P"), ("+", "-"), ("-", "+"), ("DP", "DM"), ("DM", "DP"), ("CANH", "CANL"), ("CANL", "CANH")] where net.hasSuffix(a) {
            let other = String(net.dropLast(a.count)) + b
            if ids.contains(other) { return other }
        }
        return nil
    }
    func netLength(_ net: String, in board: ECObject) -> Double {
        ecRows(board["traces"]).filter { $0["net"] as? String == net }.reduce(0) { sum, t in
            let a = ecPoint(t["start"]), b = ecPoint(t["end"])
            return sum + Double(hypot(a.x - b.x, a.y - b.y))
        }
    }
}


extension ElectronicsWorkspace {
    nonisolated static var userFootprintRoot: String { NSHomeDirectory() + "/Documents/vcad/footprints" }

    /// Write the selected footprint as a KiCad .kicad_mod in the user library,
    /// where search and placement (and KiCad itself) can find it.
    func saveFootprintToLibrary(_ model: EditorModel, name: String) {
        guard let fp = ecRows(board(model)["footprints"]).first(where: { $0["ref"] as? String == selectedRef }) else { return }
        let safe = name.replacingOccurrences(of: "[^A-Za-z0-9_.-]", with: "_", options: .regularExpression)
        guard !safe.isEmpty else { message = "Name the footprint first."; return }
        func kicadLayer(_ l: String) -> String {
            if l.count > 1, l.first == "F" || l.first == "B" { return "\(l.first!).\(l.dropFirst())" }
            return l.replacingOccurrences(of: "Cu", with: ".Cu")
        }
        var out = "(footprint \"\(safe)\"\n\t(version 20241229)\n\t(generator \"vcad\")\n\t(layer \"F.Cu\")\n"
        for g in ecRows(fp["graphics"]) where g["type"] as? String == "Line" {
            let a = ecPoint(g["start"]), b = ecPoint(g["end"])
            out += "\t(fp_line (start \(a.x) \(a.y)) (end \(b.x) \(b.y)) (stroke (width \(ecNumber(g["width"], 0.12))) (type solid)) (layer \"\(kicadLayer(g["layer"] as? String ?? "F.SilkS"))\"))\n"
        }
        for pad in ecRows(fp["pads"]) {
            let at = ecPoint(pad["position"]), s = pad["shape"] as? ECObject ?? [:], d = ecNumber(s["diameter"], 0.6)
            let kind: String = switch s["type"] as? String { case "Circle": "circle"; case "Oval": "oval"; case "RoundRect": "roundrect"; default: "rect" }
            let through = (pad["padType"] as? String ?? "") == "THT" || pad["drill"] != nil
            let layers = (pad["layers"] as? [String] ?? ["FCu"]).map { "\"\(kicadLayer($0))\"" }.joined(separator: " ")
            var line = "\t(pad \"\(pad["number"] as? String ?? "")\" \(through ? "thru_hole" : "smd") \(kind) (at \(at.x) \(at.y) \(ecNumber(pad["rotation"]))) (size \(ecNumber(s["width"], d)) \(ecNumber(s["height"], d)))"
            if through { line += " (drill \(ecNumber((pad["drill"] as? ECObject)?["diameter"], ecNumber(pad["drill"], 0.8))))" }
            line += " (layers \(layers))"
            if kind == "roundrect" { line += " (roundrect_rratio \(ecNumber(s["cornerRatio"], 0.25)))" }
            out += line + ")\n"
        }
        out += ")\n"
        let dir = Self.userFootprintRoot + "/User.pretty"
        do {
            try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
            try out.write(toFile: dir + "/\(safe).kicad_mod", atomically: true, encoding: .utf8)
            libraryItems = []
            loadLibrary()
            message = "Saved User:\(safe)"
        } catch { message = "Couldn't save: \(error.localizedDescription)" }
    }

    // MARK: symbol library

    func loadSymbols() {
        guard symbolItems.isEmpty else { return }
        Task {
            let items = await Task.detached { () -> [String] in
                let fm = FileManager.default, root = "/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols"
                var out: [String] = []
                for f in (try? fm.contentsOfDirectory(atPath: root)) ?? [] where f.hasSuffix(".kicad_sym") {
                    guard let text = try? String(contentsOfFile: root + "/" + f, encoding: .utf8) else { continue }
                    let lib = String(f.dropLast(10))
                    for line in text.split(separator: "\n") where line.hasPrefix("\t(symbol \"") {
                        let name = line.dropFirst(10).prefix { $0 != "\"" }
                        out.append("\(lib):\(name)")
                    }
                }
                return out.sorted()
            }.value
            symbolItems = items
        }
    }
    func symbolMatches() -> [String] {
        let terms = symbolSearch.lowercased().split(separator: " ")
        guard !terms.isEmpty else { return [] }
        return Array(symbolItems.lazy.filter { id in let l = id.lowercased(); return terms.allSatisfy { l.contains($0) } }.prefix(60))
    }
    func placeSymbol(_ model: EditorModel, id: String, at point: CGPoint) {
        guard let r = try? PcbFFI.librarySymbol(id) else { message = "Couldn't load \(id)."; return }
        let name = String(id.split(separator: ":").last ?? "U")
        let prefix: String
        switch true {
        case name == "R" || name.hasPrefix("R_"): prefix = "R"
        case name == "C" || name.hasPrefix("C_"): prefix = "C"
        case name == "L" || name.hasPrefix("L_"): prefix = "L"
        case id.hasPrefix("Connector"): prefix = "J"
        case name.hasPrefix("D") && id.hasPrefix("Device"): prefix = "D"
        default: prefix = "U"
        }
        let existing = ecRows(schematic(model)["components"]).compactMap { $0["ref"] as? String }
        var n = 1; while existing.contains("\(prefix)\(n)") { n += 1 }
        let ref = "\(prefix)\(n)", at = snapped(point)
        let c: ECObject = ["ref": ref, "value": name, "footprintId": r["footprint"] ?? "", "position": ecJSON(at), "rotation": 0, "mirror": false, "pins": ecRows(r["pins"])]
        model.editElectronics(name: "Place \(ref)") { doc in
            var s = sheetData(doc)
            s["components"] = ecRows(s["components"]) + [c]
            if s["nets"] == nil { s["nets"] = [String: [String]]() }
            setSheet(&doc, s)
        }
        selectedRef = ref
    }

    // MARK: sheet symbols

    /// Where each child sheet's box sits on the root: a column right of the parts.
    func sheetSymbols(_ sheet: ECObject) -> [(name: String, rect: CGRect, ports: [String])] {
        let right = ecRows(sheet["components"]).map { ecPoint($0["position"]).x }.max() ?? 0
        var y = ecRows(sheet["components"]).map { ecPoint($0["position"]).y }.min() ?? 0
        let hierarchy = sheet["hierarchy"] as? ECObject ?? [:]
        let bodies = ecRows(hierarchy["sheets"])
        let global = Set(hierarchy["global_nets"] as? [String] ?? [])
        return ecRows(hierarchy["instances"]).map { inst in
            let body = bodies.first { $0["name"] as? String == inst["sheet"] as? String }?["body"] as? ECObject
            let wired = Set((inst["connections"] as? [String: String] ?? [:]).keys)
            let ports = (body?["nets"] as? [String: [String]] ?? [:]).keys.filter { global.contains($0) || wired.contains($0) }.sorted()
            let rect = CGRect(x: right + 40, y: y, width: 44, height: max(30, CGFloat(ports.count + 2) * 5))
            y = rect.maxY + 10
            return (inst["name"] as? String ?? "sheet", rect, ports)
        }
    }
}


extension ElectronicsWorkspace {
    func editRules(_ model: EditorModel, _ change: (inout ECObject) -> Void) {
        mutateBoard(model) { b in var r = b["rules"] as? ECObject ?? [:]; change(&r); b["rules"] = r }
    }
    func addNetClass(_ model: EditorModel) {
        editRules(model) { r in
            var classes = ecRows(r["classRules"]), n = classes.count + 1
            let names = classes.compactMap { $0["name"] as? String }
            while names.contains("CLASS\(n)") { n += 1 }
            var new = r["defaultRules"] as? ECObject ?? [:]; new["name"] = "CLASS\(n)"
            classes.append(new); r["classRules"] = classes
        }
    }
    func netClass(_ net: String, in board: ECObject) -> String {
        let map = (board["rules"] as? ECObject)?["netClassAssignments"] as? [String: [String]] ?? [:]
        return map.first { $0.value.contains(net) }?.key ?? ""
    }
    func setNetClass(_ model: EditorModel, _ net: String, _ name: String) {
        editRules(model) { r in
            var map = r["netClassAssignments"] as? [String: [String]] ?? [:]
            for k in map.keys { map[k]?.removeAll { $0 == net }; if map[k]?.isEmpty == true { map[k] = nil } }
            if !name.isEmpty { map[name, default: []].append(net) }
            r["netClassAssignments"] = map
        }
    }

    /// Renumber references across every sheet, top-left to bottom-right, per
    /// prefix; the board and the netlist follow in one undo step.
    func annotate(_ model: EditorModel) {
        model.editElectronics(name: "Annotate") { doc in
            var root = doc["schematic"] as? ECObject ?? [:]
            var hierarchy = root["hierarchy"] as? ECObject ?? [:]
            var sheets = ecRows(hierarchy["sheets"])
            func parts(_ s: ECObject) -> [ECObject] { ecRows(s["components"]) }
            var all = parts(root)
            for sheet in sheets { all += parts(sheet["body"] as? ECObject ?? [:]) }
            let prefix = { (r: String) in String(r.prefix { $0.isLetter }) }
            var counts: [String: Int] = [:], renames: [String: String] = [:]
            let ordered = all.sorted {
                let a = ecPoint($0["position"]), b = ecPoint($1["position"])
                return a.y != b.y ? a.y < b.y : a.x < b.x
            }
            for c in ordered {
                guard let ref = c["ref"] as? String, !ref.hasPrefix("#"), ref.last?.isNumber == true else { continue }
                let p = prefix(ref); counts[p, default: 0] += 1
                renames[ref] = "\(p)\(counts[p]!)"
            }
            func apply(_ s: inout ECObject) {
                s["components"] = parts(s).map { c in var c = c
                    if let r = c["ref"] as? String, let n = renames[r] { c["ref"] = n }
                    return c }
                let nets = s["nets"] as? [String: [String]] ?? [:]
                s["nets"] = nets.mapValues { pins in pins.map { pin in
                    guard let dot = pin.firstIndex(of: "."), let n = renames[String(pin[..<dot])] else { return pin }
                    return n + pin[dot...] } }
            }
            apply(&root)
            for i in sheets.indices { var b = sheets[i]["body"] as? ECObject ?? [:]; apply(&b); sheets[i]["body"] = b }
            if !sheets.isEmpty { hierarchy["sheets"] = sheets; root["hierarchy"] = hierarchy }
            doc["schematic"] = root
            var nodes = doc["nodes"] as? ECObject ?? [:]
            for (id, value) in nodes {
                guard var node = value as? ECObject, var op = node["op"] as? ECObject, var b = op["board"] as? ECObject else { continue }
                b["footprints"] = ecRows(b["footprints"]).map { f in var f = f
                    if let r = f["ref"] as? String, let n = renames[r] { f["ref"] = n }
                    return f }
                op["board"] = b; node["op"] = op; nodes[id] = node
            }
            doc["nodes"] = nodes
        }
        selectedRef = nil; multiRefs = []
    }
}
