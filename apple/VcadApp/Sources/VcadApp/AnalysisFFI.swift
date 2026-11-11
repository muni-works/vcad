import Foundation
import CVcadFFI

// Swift side of the analysis C ABI (crates/vcad-ffi/src/{dfm,fea,clearance}.rs).
// Every call takes the document IR as JSON and returns owned JSON, decoded
// here into plain value types. All of it is pure and thread-agnostic, so the
// store runs it off the main actor.

enum AnalysisFFI {
    struct Failure: Error, CustomStringConvertible {
        let message: String
        var description: String { message }
    }

    /// Run one `char *f(const char *doc, const char *req)` entry point.
    static func call(_ f: (UnsafePointer<CChar>?, UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?,
                     doc: Data, request: Data?) throws -> Data {
        let docStr = String(decoding: doc, as: UTF8.self)
        let reqStr = request.map { String(decoding: $0, as: UTF8.self) }
        let raw: UnsafeMutablePointer<CChar>? = docStr.withCString { d in
            if let reqStr { return reqStr.withCString { r in f(d, r) } }
            return f(d, nil)
        }
        guard let raw else { throw Failure(message: lastError() ?? "analysis failed") }
        defer { vcad_string_free(raw) }
        return Data(bytes: raw, count: strlen(raw))
    }

    /// Thread-local: read it on the thread that made the failing call.
    static func lastError() -> String? {
        var len = 0
        guard let e = vcad_last_error(&len), len > 0 else { return nil }
        return String(decoding: UnsafeBufferPointer(start: e, count: len), as: UTF8.self)
    }

    /// `vcad_fea_solve` as a job: `step(fraction, level, levels, solving)` runs
    /// before each mesh and solve; returning false cancels the study.
    static func feaSolve(doc: Data, request: Data,
                         step: @escaping (Double, Int, Int, Bool) -> Bool) throws -> Data {
        final class Box { let step: (Double, Int, Int, Bool) -> Bool; init(_ s: @escaping (Double, Int, Int, Bool) -> Bool) { step = s } }
        let box = Unmanaged.passRetained(Box(step))
        defer { box.release() }
        let hook: vcad_fea_progress_fn = { ctx, f, level, levels, solving in
            guard let ctx else { return true }
            return Unmanaged<Box>.fromOpaque(ctx).takeUnretainedValue().step(f, Int(level), Int(levels), solving)
        }
        let ctx = box.toOpaque()
        return try call({ d, r in vcad_fea_solve_progress(d, r, hook, ctx) }, doc: doc, request: request)
    }

    static func encode(_ obj: Any) throws -> Data {
        try JSONSerialization.data(withJSONObject: obj, options: [.sortedKeys])
    }

    // MARK: DFM

    static func dfmCheck(doc: Data, process: String) throws -> DfmReport {
        let out = try call(vcad_dfm_check, doc: doc, request: try encode(["process": process]))
        return try JSONDecoder().decode(DfmReport.self, from: out)
    }

    /// The patched document IR.
    static func dfmApply(doc: Data, patch: DfmPatch) throws -> [String: Any] {
        let out = try call(vcad_dfm_apply_patch, doc: doc, request: try JSONEncoder().encode(patch))
        guard let json = try JSONSerialization.jsonObject(with: out) as? [String: Any] else {
            throw Failure(message: "patched document is not an object")
        }
        return json
    }

    static let dfmProcesses: [String] = {
        guard let raw = vcad_dfm_processes() else { return ["fdm"] }
        defer { vcad_string_free(raw) }
        let obj = (try? JSONSerialization.jsonObject(with: Data(bytes: raw, count: strlen(raw)))) as? [String: Any]
        return obj?["processes"] as? [String] ?? ["fdm"]
    }()

    // MARK: FEA

    static func feaSolve(doc: Data, request: FeaRequest) throws -> FeaResult {
        let out = try call(vcad_fea_solve, doc: doc, request: try JSONEncoder().encode(request))
        return try JSONDecoder().decode(FeaResult.self, from: out)
    }

    static let feaMaterials: [FeaMaterial] = {
        guard let raw = vcad_fea_materials() else { return [] }
        defer { vcad_string_free(raw) }
        return (try? JSONDecoder().decode([FeaMaterial].self, from: Data(bytes: raw, count: strlen(raw)))) ?? []
    }()

    // MARK: Clearance

    static func clearance(doc: Data, includeMesh: Bool = true) throws -> ClearanceReport {
        let out = try call(vcad_clearance, doc: doc, request: try encode(["include_mesh": includeMesh]))
        return try JSONDecoder().decode(ClearanceReport.self, from: out)
    }
}

// MARK: - DFM types

enum DfmPatch: Codable, Equatable {
    case setParameter(name: String, from: Double, to: Double)
    case setField(node: UInt64, path: String, from: Double, to: Double)

    private enum K: String, CodingKey { case kind, name, node, path, from, to }
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: K.self)
        let from = try c.decode(Double.self, forKey: .from), to = try c.decode(Double.self, forKey: .to)
        switch try c.decode(String.self, forKey: .kind) {
        case "set_parameter": self = .setParameter(name: try c.decode(String.self, forKey: .name), from: from, to: to)
        default: self = .setField(node: try c.decode(UInt64.self, forKey: .node),
                                  path: try c.decode(String.self, forKey: .path), from: from, to: to)
        }
    }
    func encode(to e: Encoder) throws {
        var c = e.container(keyedBy: K.self)
        switch self {
        case .setParameter(let n, let f, let t):
            try c.encode("set_parameter", forKey: .kind); try c.encode(n, forKey: .name)
            try c.encode(f, forKey: .from); try c.encode(t, forKey: .to)
        case .setField(let n, let p, let f, let t):
            try c.encode("set_field", forKey: .kind); try c.encode(n, forKey: .node); try c.encode(p, forKey: .path)
            try c.encode(f, forKey: .from); try c.encode(t, forKey: .to)
        }
    }

    /// "Set wall 0.5 → 0.8 mm" / "Set size.z on #3 0.5 → 0.8 mm".
    var summary: String {
        func n(_ v: Double) -> String { String(format: v.rounded() == v ? "%.0f" : "%.2f", v) }
        switch self {
        case .setParameter(let name, let f, let t): return "Set \(name) \(n(f)) → \(n(t)) mm"
        case .setField(let node, let path, let f, let t): return "Set \(path) on #\(node) \(n(f)) → \(n(t)) mm"
        }
    }
}

struct DfmIssue: Decodable, Identifiable, Equatable {
    let id: String
    let rule: String
    let severity: String
    let message: String
    let explanation: String
    let anchor: [Double]
    let measured: Double
    let limit: Double
    let units: String
    let part_index: Int
    let part_name: String
    let face_ids: [UInt32]
    let patch: DfmPatch?
    let patch_note: String?

    var anchorF: SIMD3<Float> {
        anchor.count >= 3 ? SIMD3(Float(anchor[0]), Float(anchor[1]), Float(anchor[2])) : .zero
    }
    /// "Thin wall" from "fdm.thin_wall".
    var ruleTitle: String {
        let tail = rule.split(separator: ".").last.map(String.init) ?? rule
        return tail.replacingOccurrences(of: "_", with: " ").capitalized
    }
}

struct DfmReport: Decodable, Equatable {
    struct Counts: Decodable, Equatable { let error: Int; let warning: Int; let info: Int }
    let process: String
    let rule_pack_name: String
    let issues: [DfmIssue]
    let counts: Counts
    let score: Int
    let skipped_parts: [String]
}

// MARK: - FEA types

struct FeaMaterial: Decodable, Identifiable, Hashable {
    let id: String
    let label: String
    let youngs_modulus_mpa: Double
    let poisson: Double
    let yield_strength_mpa: Double
}

struct FeaRegion: Codable, Equatable {
    var min: [Double]
    var max: [Double]
}

struct FeaRequest: Encodable {
    struct Load: Encodable { var faces: [UInt32]; var force: [Double] }
    struct Support: Encodable { var faces: [UInt32]; var fix: [Bool] }
    var part_index: Int
    var material: String?
    var resolution: Int
    var levels: Int
    var loads: [Load]
    var supports: [Support]
    var fields: Bool = true
}

struct FeaResult: Decodable {
    struct Field: Decodable {
        let dims_zyx: [Int]
        let origin_mm: [Double]
        let voxel_mm: [Double]
        let von_mises_mpa: [Float]
        let displacement_mm: [Float]
        let mask: [UInt8]
        let von_mises_range: [Double]
        let displacement_range: [Double]
    }
    struct Study: Decodable { let stress_change_rel: Double; let displacement_change_rel: Double }
    let part_index: Int
    let part_name: String
    let material: String
    let study: Study
    let youngs_modulus_mpa: Double
    let yield_strength_mpa: Double?
    let max_von_mises_mpa: Double
    let max_stress_at: [Double]
    let max_displacement_mm: Double
    let max_displacement_at: [Double]
    let safety_factor: Double?
    let converged: Bool
    let reasons: [String]
    let load_regions: [FeaRegion]
    let support_regions: [FeaRegion]
    let notes: [String]
    let field: Field?
}

// MARK: - Clearance types

struct ClearancePair: Decodable, Identifiable {
    struct Mesh: Decodable { let positions: [Float]; let indices: [UInt32] }
    var id: String { "\(a)-\(b)" }
    let a: Int
    let b: Int
    let a_name: String
    let b_name: String
    let distance_mm: Double
    let intersecting: Bool
    let overlap_volume_mm3: Double
    let point_a: [Double]
    let point_b: [Double]
    let status: String
    let clash: Mesh?
}

struct ClearanceReport: Decodable {
    let parts: [String]
    let pairs: [ClearancePair]
    let interference_count: Int
    let min_clearance_mm: Double?
    let total_overlap_mm3: Double
    let skipped_parts: [String]
}
