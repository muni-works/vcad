import Foundation
import CVcadFFI

// Swift side of the electronics C ABI (crates/vcad-ffi/src/{pcb,schematic}.rs).
// Boards and sheets cross as IR JSON (the same dictionaries the document stores),
// results come back as JSON objects. Everything is pure, so the store runs it
// off the main actor.

enum PcbFFI {
    struct Failure: Error, CustomStringConvertible {
        let message: String
        var description: String { message }
    }

    typealias Obj = [String: Any]

    private static func finish(_ raw: UnsafeMutablePointer<CChar>?, _ what: String) throws -> Obj {
        guard let raw else { throw Failure(message: AnalysisFFI.lastError() ?? "\(what) failed") }
        defer { vcad_string_free(raw) }
        let data = Data(bytes: raw, count: strlen(raw))
        guard let obj = try JSONSerialization.jsonObject(with: data) as? Obj else {
            throw Failure(message: "\(what): result is not an object")
        }
        return obj
    }

    private static func text(_ obj: Any) throws -> String {
        String(decoding: try JSONSerialization.data(withJSONObject: obj, options: [.sortedKeys]), as: UTF8.self)
    }

    static func fillZones(_ pcb: Obj) throws -> Obj {
        let json = try text(pcb)
        return try finish(json.withCString { vcad_pcb_fill_zones($0) }, "fill zones")
    }
    static func libraryFootprint(_ req: Obj) throws -> Obj {
        let json = try text(req)
        return try finish(json.withCString { vcad_pcb_library_footprint($0) }, "footprint")
    }
    static func routeDiffPair(_ pcb: Obj, p: String, n: String) throws -> Obj {
        try call2(vcad_pcb_route_diff_pair, pcb, ["p": p, "n": n], "route pair")
    }
    static func librarySymbol(_ id: String) throws -> Obj {
        let json = try text(["id": id])
        return try finish(json.withCString { vcad_pcb_library_symbol($0) }, "symbol")
    }
    static func ratsnest(_ pcb: Obj) throws -> Obj {
        let json = try text(pcb)
        return try finish(json.withCString { vcad_pcb_ratsnest($0) }, "ratsnest")
    }

    /// A board open for hand routing (`vcad_pcb_router_*`). Queries are
    /// serialised by the Rust side's lock.
    final class Router: @unchecked Sendable {
        private let handle: OpaquePointer
        init(board: Obj) throws {
            let json = try PcbFFI.text(board)
            guard let h = json.withCString({ vcad_pcb_router_open($0) }) else {
                throw Failure(message: AnalysisFFI.lastError() ?? "router open failed")
            }
            handle = h
        }
        deinit { vcad_pcb_router_free(handle) }
        private func ask(_ f: (OpaquePointer?, UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?, _ req: Obj, _ what: String) throws -> Obj {
            let text = try PcbFFI.text(req)
            return try PcbFFI.finish(text.withCString { f(handle, $0) }, what)
        }
        func route(_ req: Obj) throws -> Obj { try ask(vcad_pcb_router_route, req, "route") }
        func commit(_ req: Obj) throws -> Obj { try ask(vcad_pcb_router_commit, req, "commit") }
    }

    static func call2(_ f: (UnsafePointer<CChar>?, UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?,
                      _ a: Any, _ req: Obj, _ what: String) throws -> Obj {
        let sa = try text(a), sr = try text(req)
        return try finish(sa.withCString { pa in sr.withCString { pr in f(pa, pr) } }, what)
    }

    static func call3(_ f: (UnsafePointer<CChar>?, UnsafePointer<CChar>?, UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?,
                      _ a: Any, _ b: Any, _ req: Obj, _ what: String) throws -> Obj {
        let sa = try text(a), sb = try text(b), sr = try text(req)
        return try finish(sa.withCString { pa in sb.withCString { pb in sr.withCString { pr in f(pa, pb, pr) } } }, what)
    }

    static func importBoard(path: String) throws -> Obj {
        try finish(path.withCString { vcad_pcb_import($0) }, "import")
    }
    static func drc(_ pcb: Obj) throws -> Obj { try call2(vcad_pcb_drc, pcb, [:], "DRC") }
    /// One connection per search window: a joint window lets one impossible
    /// connection report its whole cluster infeasible (FOC board: 18 of 169
    /// routed with the default of 6, against 94 of 116 one at a time).
    static func autoroute(_ pcb: Obj) throws -> Obj {
        try call2(vcad_pcb_autoroute, pcb, ["max_cluster": 1, "budget": 20_000_000], "autoroute")
    }
    static func fixDrc(_ pcb: Obj, calibrate: Bool) throws -> Obj {
        try call2(vcad_pcb_fix_drc, pcb, ["calibrate": calibrate], "fix DRC")
    }
    static func exportGerbers(_ pcb: Obj, dir: String) throws -> Obj {
        try call2(vcad_pcb_export_gerbers, pcb, ["dir": dir], "Gerber export")
    }
    static func netLengths(_ pcb: Obj) throws -> Obj { try call2(vcad_pcb_length_match, pcb, [:], "lengths") }
    static func lengthMatch(_ pcb: Obj, nets: [String], target: Double?, tolerance: Double) throws -> Obj {
        var req: Obj = ["nets": nets, "tolerance": tolerance]
        if let target { req["target"] = target }
        return try call2(vcad_pcb_length_match, pcb, req, "length tuning")
    }
    static func enclosureFit(doc: Obj, pcb: Obj, part: Int, placement: Obj?) throws -> Obj {
        var req: Obj = ["part": part]
        if let placement { req["placement"] = placement }
        return try call3(vcad_pcb_enclosure_fit, doc, pcb, req, "fit check")
    }
    static func boardFromSolid(doc: Obj, part: Int) throws -> Obj {
        try call2(vcad_pcb_board_from_solid, doc, ["part": part], "board from solid")
    }
    static func solidFromBoard(doc: Obj, pcb: Obj, z: Double) throws -> Obj {
        try call3(vcad_pcb_solid_from_board, doc, pcb, ["z": z], "solid from board")
    }
    static func enclosureForBoard(doc: Obj, pcb: Obj) throws -> Obj {
        try call3(vcad_pcb_enclosure_for_board, doc, pcb, [:], "enclose board")
    }
    static func simulate(_ sheet: Obj, ground: String?, tstop: Double?) throws -> Obj {
        var req: Obj = [:]
        if let ground, !ground.isEmpty { req["ground"] = ground }
        if let tstop, tstop > 0 { req["tstop"] = tstop }
        return try call2(vcad_schematic_simulate, sheet, req, "simulation")
    }
    static func erc(_ sheet: Obj) throws -> Obj {
        let s = try text(sheet)
        return try finish(s.withCString { vcad_schematic_erc($0) }, "ERC")
    }
}
