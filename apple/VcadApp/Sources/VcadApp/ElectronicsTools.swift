import SwiftUI
import AppKit
import UniformTypeIdentifiers

/// A JSON object handed across the detached-task boundary. The FFI results are
/// freshly decoded and never shared, so crossing actors with them is safe.
struct ECBox: @unchecked Sendable { let value: ECObject }

/// Kernel-backed electronics operations (crates/vcad-ffi/src/pcb.rs,
/// schematic.rs): KiCad import, real DRC with the route-attributable delta,
/// autoroute, the fab-prep fix loop, Gerber export behind the clean-DRC gate,
/// length tuning, enclosure co-design and circuit simulation.
///
/// Every op reads the board/document from the model, runs off the main actor,
/// and writes back through `ElectronicsWorkspace.mutateBoard` /
/// `EditorModel.editElectronics`, so each is one undo step.
@MainActor @Observable
final class ElectronicsTools {
    enum Sheet: String, Identifiable { case drc, erc, footprint, symbol, length, stackup, netClasses, simulate, enclosure; var id: String { rawValue } }

    var busy: String?
    var sheet: Sheet?
    var error: String?
    var status: String?

    // DRC
    var drc: ECObject?
    // Fix / route narration
    var fixSummary: String?
    // Length tuning
    var lengths: [(String, Double)] = []
    var tuneNets: Set<String> = []
    var tuneTarget: Double?
    var tuneTolerance = 0.5
    var tuneResult: [ECObject] = []
    // Enclosure
    var enclosurePart = 0
    var placement: ECObject?
    var fit: ECObject?
    // Simulation
    var groundNet = ""
    var tstop = 0.0
    var sim: ECObject?
    var erc: [String] = []

    private func run(_ label: String, _ work: @escaping @Sendable () throws -> ECBox,
                     then: @escaping @MainActor (ECObject) -> Void) {
        guard busy == nil else { return }
        busy = label; error = nil
        Task {
            let r = await Task.detached { Result { try work() } }.value
            busy = nil
            switch r {
            case .success(let box): then(box.value)
            case .failure(let e): error = "\(label): \(e)"
            }
        }
    }

    private func boardBox(_ ec: ElectronicsWorkspace, _ model: EditorModel) -> ECBox? {
        let b = ec.board(model)
        guard !b.isEmpty else { error = "No board in this document — create or import one first."; return nil }
        return ECBox(value: b)
    }

    private func adopt(board: Any?, _ ec: ElectronicsWorkspace, _ model: EditorModel) {
        guard let board = board as? ECObject else { return }
        ec.mutateBoard(model) { $0 = board }
    }

    // MARK: board

    func importBoard(_ ec: ElectronicsWorkspace, _ model: EditorModel) {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = ["kicad_pcb", "brd"].compactMap { UTType(filenameExtension: $0) }
        panel.message = "Choose a KiCad (.kicad_pcb) or Eagle (.brd) board"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        importBoard(url, ec, model)
    }

    /// Import a KiCad/Eagle board file into this document's board.
    func importBoard(_ url: URL, _ ec: ElectronicsWorkspace, _ model: EditorModel) {
        let path = url.path
        run("Import") { ECBox(value: try PcbFFI.importBoard(path: path)) } then: { [self] r in
            if ec.board(model).isEmpty { ec.createBoard(model) }
            guard !ec.board(model).isEmpty else { error = ec.message ?? "Couldn't add a board to this document."; return }
            adopt(board: r["pcb"], ec, model)
            let warns = (r["warnings"] as? [String])?.count ?? 0
            status = "Imported \(url.lastPathComponent): \(ecNumber(r["footprints"]).formatted()) parts, \(ecNumber(r["nets"]).formatted()) nets, \(ecNumber(r["traces"]).formatted()) traces" + (warns > 0 ? " · \(warns) warning(s)" : "")
            drc = nil; fit = nil
        }
    }

    func runDrc(_ ec: ElectronicsWorkspace, _ model: EditorModel) {
        guard let b = boardBox(ec, model) else { return }
        run("DRC") { ECBox(value: try PcbFFI.drc(b.value)) } then: { [self] r in
            drc = r; sheet = .drc
        }
    }

    func autoroute(_ ec: ElectronicsWorkspace, _ model: EditorModel) {
        guard let b = boardBox(ec, model) else { return }
        run("Autoroute") { ECBox(value: try PcbFFI.autoroute(b.value)) } then: { [self] r in
            adopt(board: r["pcb"], ec, model)
            drc = r["drc"] as? ECObject
            let unrouted = ecNumber(drc?["unrouted"])
            let v = r["verdict"] as? ECObject
            let routed = Int(ecNumber(v?["routed"])) + Int(ecNumber(r["finished"]))
            status = "Routed \(routed) · \(Int(unrouted)) nets open · \(Int(ecNumber(drc?["route_attributable"]))) route DRC"
        }
    }

    func fixDrc(_ ec: ElectronicsWorkspace, _ model: EditorModel, calibrate: Bool = false) {
        guard let b = boardBox(ec, model) else { return }
        run("Fix DRC") { ECBox(value: try PcbFFI.fixDrc(b.value, calibrate: calibrate)) } then: { [self] r in
            adopt(board: r["pcb"], ec, model)
            drc = r["drc"] as? ECObject
            fixSummary = r["summary"] as? String
            status = r["headline"] as? String
            sheet = .drc
        }
    }

    func exportGerbers(_ ec: ElectronicsWorkspace, _ model: EditorModel) {
        guard let b = boardBox(ec, model) else { return }
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true; panel.canChooseFiles = false; panel.canCreateDirectories = true
        panel.prompt = "Export"; panel.message = "Choose a folder for Gerbers, drills, BOM and pick-and-place"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let dir = url.path
        run("Gerber export") { ECBox(value: try PcbFFI.exportGerbers(b.value, dir: dir)) } then: { [self] r in
            let n = (r["files"] as? [String])?.count ?? 0
            status = "Exported \(n) fabrication files to \(url.lastPathComponent)"
            NSWorkspace.shared.activateFileViewerSelecting([url])
        }
    }

    // MARK: length tuning

    func openLengths(_ ec: ElectronicsWorkspace, _ model: EditorModel) {
        guard let b = boardBox(ec, model) else { return }
        run("Lengths") { ECBox(value: try PcbFFI.netLengths(b.value)) } then: { [self] r in
            lengths = (r["lengths"] as? [[Any]] ?? []).compactMap { row in
                guard row.count == 2, let n = row[0] as? String else { return nil }
                return (n, ecNumber(row[1]))
            }
            tuneResult = []
            sheet = .length
        }
    }

    func tune(_ ec: ElectronicsWorkspace, _ model: EditorModel) {
        guard let b = boardBox(ec, model), !tuneNets.isEmpty else { return }
        let nets = tuneNets.sorted(), target = tuneTarget, tol = tuneTolerance
        run("Length tuning") { ECBox(value: try PcbFFI.lengthMatch(b.value, nets: nets, target: target, tolerance: tol)) } then: { [self] r in
            adopt(board: r["pcb"], ec, model)
            tuneResult = ecRows((r["result"] as? ECObject)?["nets"])
            drc = r["drc"] as? ECObject
            status = (r["result"] as? ECObject)?["all_matched"] as? Bool == true ? "All selected nets matched" : "Some nets could not be matched — see Length Tuning"
        }
    }

    func stitch(_ ec: ElectronicsWorkspace, _ model: EditorModel, _ net: String) {
        guard let b = boardBox(ec, model) else { return }
        run("Stitching") { ECBox(value: try PcbFFI.stitchZones(b.value, net: net, pitch: 5)) } then: { [self] r in
            adopt(board: r["pcb"], ec, model)
            status = r["reason"] as? String ?? "Added \(r["added"] as? Int ?? 0) stitching vias on \(net)"
        }
    }

    // MARK: enclosure

    /// Visible roots, in evaluated-part order, for the enclosure picker.
    func partNames(_ model: EditorModel) -> [String] {
        let nodes = model.documentJSON?["nodes"] as? ECObject ?? [:]
        return ecRows(model.documentJSON?["roots"]).filter { $0["visible"] as? Bool != false }.enumerated().map { i, r in
            let id = (r["root"] as? NSNumber).map { "\($0.intValue)" } ?? ""
            return ((nodes[id] as? ECObject)?["name"] as? String) ?? "Part \(i + 1)"
        }
    }

    private func docBox(_ model: EditorModel) -> ECBox? {
        guard let d = model.documentJSON else { error = "Open an editable vcad document first."; return nil }
        return ECBox(value: d)
    }

    func encloseBoard(_ ec: ElectronicsWorkspace, _ model: EditorModel) {
        guard let b = boardBox(ec, model), let d = docBox(model) else { return }
        run("Enclose board") { ECBox(value: try PcbFFI.enclosureForBoard(doc: d.value, pcb: b.value)) } then: { [self] r in
            guard let doc = r["document"] as? ECObject else { return }
            model.editElectronics(name: "Enclose Board") { $0 = doc }
            placement = r["placement"] as? ECObject
            enclosurePart = Int(ecNumber(r["part"]))
            fit = nil
            status = "Enclosure added around the board · run Fit Check to verify"
            sheet = .enclosure
        }
    }

    func fitCheck(_ ec: ElectronicsWorkspace, _ model: EditorModel) {
        guard let b = boardBox(ec, model), let d = docBox(model) else { return }
        let part = enclosurePart
        run("Fit check") { ECBox(value: try PcbFFI.enclosureFit(doc: d.value, pcb: b.value, part: part, placement: nil)) } then: { [self] r in
            fit = r["report"] as? ECObject
            status = fit?["summary"] as? String
            sheet = .enclosure
        }
    }

    func boardFromSolid(_ ec: ElectronicsWorkspace, _ model: EditorModel) {
        guard let d = docBox(model) else { return }
        let part = enclosurePart
        run("Board from solid") { ECBox(value: try PcbFFI.boardFromSolid(doc: d.value, part: part)) } then: { [self] r in
            if ec.board(model).isEmpty { ec.createBoard(model) }
            adopt(board: r["pcb"], ec, model)
            placement = r["placement"] as? ECObject
            status = "Board outline and mounting holes derived from the cavity"
        }
    }

    func solidFromBoard(_ ec: ElectronicsWorkspace, _ model: EditorModel) {
        guard let b = boardBox(ec, model), let d = docBox(model) else { return }
        let z = ecNumber((placement?["offset"] as? ECObject)?["z"])
        run("Board to solid") { ECBox(value: try PcbFFI.solidFromBoard(doc: d.value, pcb: b.value, z: z)) } then: { r in
            model.editElectronics(name: "Board to Solid") { $0 = r }
        }
    }

    // MARK: simulation

    /// ERC findings with positions, kept for the sheet's markers.
    var ercFindings: [ECObject] = []
    func runErc(_ ec: ElectronicsWorkspace, _ model: EditorModel) {
        let sheetJSON = ec.rootSchematic(model)
        guard !sheetJSON.isEmpty else { error = "This document has no schematic."; return }
        let s = ECBox(value: sheetJSON)
        run("ERC") { ECBox(value: try PcbFFI.erc(s.value)) } then: { [self] r in
            ercFindings = ecRows(r["violations"])
            sheet = .erc
        }
    }

    func routePair(_ ec: ElectronicsWorkspace, _ model: EditorModel, _ p: String, _ n: String) {
        guard let b = boardBox(ec, model) else { return }
        run("Route pair") { ECBox(value: try PcbFFI.routeDiffPair(b.value, p: p, n: n)) } then: { [self] r in
            adopt(board: r["pcb"], ec, model)
            status = (r["routed"] as? Bool) == true ? "Routed pair \(p) / \(n)\((r["coupled"] as? Bool) == true ? " coupled" : ", each leg separately")" : (r["reason"] as? String ?? "Pair not routed")
        }
    }

    func simulate(_ ec: ElectronicsWorkspace, _ model: EditorModel) {
        let sheetJSON = ec.schematic(model)
        guard !sheetJSON.isEmpty else { error = "This document has no schematic."; return }
        let s = ECBox(value: sheetJSON), g = groundNet, t = tstop
        run("Simulate") {
            let erc = try PcbFFI.erc(s.value)
            var out = try PcbFFI.simulate(s.value, ground: g, tstop: t)
            out["erc"] = erc
            return ECBox(value: out)
        } then: { [self] r in
            sim = r
            erc = ecRows((r["erc"] as? ECObject)?["violations"]).compactMap { $0["message"] as? String }
        }
    }
}
