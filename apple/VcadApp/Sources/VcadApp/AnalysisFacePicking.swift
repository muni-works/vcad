import Foundation
import CVcadFFI

// Face picking for the analysis panel (FEA loads / supports).
//
// The viewport face picker is the M1 lane's public picking API; until it is
// on main, analysis reads faces through this protocol. The default source is
// a face table straight from the kernel (`vcad_topology_json`): every face of
// the part with its surface kind, centre, normal and area, so a load can be
// put on "the +X planar face at x = 100" precisely. When the viewport picker
// lands, an adapter conforming to `AnalysisFacePicker` feeds the same ids —
// face id = shell ordinal everywhere — and nothing else here changes.

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
    /// Faces of `part` the user can pick from (nil when the part has no B-rep).
    func faces(part: Int, document: Data) -> [AnalysisFace]?
    /// Faces the user has currently selected on `part` in the viewport, if
    /// this source has a viewport selection (the table source does not).
    func viewportSelection(part: Int) -> [UInt32]
}

/// Kernel face table, cached per document bytes.
@MainActor
final class KernelFaceTable: AnalysisFacePicker {
    private var key: Data?
    private var cache: [Int: [AnalysisFace]] = [:]

    func faces(part: Int, document data: Data) -> [AnalysisFace]? {
        if data != key { key = data; cache.removeAll() }
        if let c = cache[part] { return c }
        let scene: OpaquePointer? = data.withUnsafeBytes { raw in
            vcad_scene_from_json(raw.bindMemory(to: UInt8.self).baseAddress, data.count)
        }
        guard let scene else { return nil }
        defer { vcad_scene_free(scene) }
        guard part >= 0, part < vcad_scene_part_count(scene), let solid = vcad_scene_part_solid(scene, part) else {
            return nil
        }
        defer { vcad_solid_free(solid) }
        guard let raw = vcad_topology_json(solid) else { return nil }
        defer { vcad_string_free(raw) }
        struct Report: Decodable { let faces: [AnalysisFace] }
        let faces = (try? JSONDecoder().decode(Report.self, from: Data(bytes: raw, count: strlen(raw))))?.faces
        cache[part] = faces
        return faces
    }

    func viewportSelection(part: Int) -> [UInt32] { [] }
}
