import Foundation
import CVcadFFI

// Face table for the analysis panel (FEA loads / supports).
//
// Viewport picking is M1's PickingState (Picking.swift): arming a load or
// support row (`AnalysisStore.setPickTarget`) switches `model.picking` to
// faces and mirrors its selection into the row. This kernel face table
// (`vcad_topology_json`) is the precise alternative listed under the row —
// "the +X face at x = 100" — with the same ids (shell ordinals).

/// One face of a part, as the analysis panel lists it.
struct AnalysisFace: Decodable, Identifiable, Hashable {
    let id: UInt32
    let surface: String
    let center: [Double]
    let normal: [Double]
    let area: Double

    var isPlanar: Bool { surface.lowercased().hasPrefix("plan") }

    /// "+X plane at x 100.0" for axis-aligned planes, else "Cylinder near (…)".
    var label: String {
        func f(_ v: Double) -> String { String(format: "%.1f", v) }
        if isPlanar, normal.count == 3, let a = (0..<3).first(where: { abs(normal[$0]) > 0.999 }) {
            let axis = ["X", "Y", "Z"][a]
            return "\(normal[a] > 0 ? "+" : "−")\(axis) face at \(axis.lowercased()) \(f(center[a]))"
        }
        let c = center.count == 3 ? "(\(f(center[0])), \(f(center[1])), \(f(center[2])))" : ""
        return "\(surface) near \(c)"
    }
}

/// Where the analysis panel gets picked faces from.
@MainActor
protocol AnalysisFacePicker: AnyObject {
    /// Faces of `part` already loaded for these bytes (never evaluates):
    /// nil = not loaded yet; `.some(nil)` = the part has no B-rep.
    func cachedFaces(part: Int, document: Data) -> [AnalysisFace]??
    /// Faces of `part` the user can pick from (nil when the part has no
    /// B-rep), evaluated off the main thread on a miss.
    func faces(part: Int, document: Data) async -> [AnalysisFace]?
    /// Faces the user has currently selected on `part` in the viewport, if
    /// this source has a viewport selection (the table source does not).
    func viewportSelection(part: Int) -> [UInt32]
}

/// Kernel face table, cached per document bytes. A miss evaluates through
/// picking's shared incremental session (`PickingState.evaluate`) on a
/// background thread — usually a cache hit there, since the frame queue or
/// picking's prewarm already evaluated these bytes — never on main.
@MainActor
final class KernelFaceTable: AnalysisFacePicker {
    private var key: Data?
    private var cache: [Int: [AnalysisFace]?] = [:]

    func cachedFaces(part: Int, document data: Data) -> [AnalysisFace]?? {
        data == key ? cache[part] : nil
    }

    func faces(part: Int, document data: Data) async -> [AnalysisFace]? {
        if let c = cachedFaces(part: part, document: data) { return c }
        let faces = await Task.detached(priority: .userInitiated) { Self.load(part: part, document: data) }.value
        if data != key { key = data; cache.removeAll() }
        cache[part] = .some(faces)
        return faces
    }

    nonisolated private static func load(part: Int, document data: Data) -> [AnalysisFace]? {
        guard let scene = PickingState.evaluate(data) else { return nil }
        defer { vcad_scene_free(scene) }
        guard part >= 0, part < vcad_scene_part_count(scene), let solid = vcad_scene_part_solid(scene, part) else {
            return nil
        }
        defer { vcad_solid_free(solid) }
        guard let raw = vcad_topology_json(solid) else { return nil }
        defer { vcad_string_free(raw) }
        struct Report: Decodable { let faces: [AnalysisFace] }
        return (try? JSONDecoder().decode(Report.self, from: Data(bytes: raw, count: strlen(raw))))?.faces
    }

    func viewportSelection(part: Int) -> [UInt32] { [] }
}
