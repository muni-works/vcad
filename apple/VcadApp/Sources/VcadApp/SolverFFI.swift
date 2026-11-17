import Foundation
import CVcadFFI

// Swift side of `vcad_solver_run` (crates/vcad-ffi/src/solver.rs → the
// vcad-solver-tools crate): the same implementation the agent's
// simulate_charged_particles / optimize_electrodes / simulate_neutron_shield
// / compare_measurements tools run, so the Fields panel and the agent can
// never disagree about a number. Pure and thread-agnostic: run it off main.

enum SolverFFI {
    /// Run a solver tool; `step(fraction, message)` runs on the calling
    /// thread before each particle / evaluation and returns false to cancel.
    static func run(_ name: String, args: Data,
                    step: @escaping (Double, String) -> Bool = { _, _ in true }) throws -> Data {
        final class Box { let step: (Double, String) -> Bool; init(_ s: @escaping (Double, String) -> Bool) { step = s } }
        let box = Unmanaged.passRetained(Box(step))
        defer { box.release() }
        let hook: vcad_solver_progress_fn = { ctx, f, msg in
            guard let ctx else { return true }
            let m = msg.map { String(cString: $0) } ?? ""
            return Unmanaged<Box>.fromOpaque(ctx).takeUnretainedValue().step(f, m)
        }
        let argStr = String(decoding: args, as: UTF8.self)
        let raw: UnsafeMutablePointer<CChar>? = name.withCString { n in
            argStr.withCString { a in vcad_solver_run(n, a, hook, box.toOpaque()) }
        }
        guard let raw else { throw AnalysisFFI.Failure(message: AnalysisFFI.lastError() ?? "\(name) failed") }
        defer { vcad_string_free(raw) }
        return Data(bytes: raw, count: strlen(raw))
    }
}

// MARK: - Result types (decoded subsets; the raw JSON is kept alongside)

struct SolverClaim: Decodable, Identifiable, Hashable {
    let name: String
    let value: Double
    let unit: String
    let note: String?
    let rse: Double?
    var id: String { name }
}

struct ParticleRun: Decodable {
    struct Stats: Decodable {
        let n: Int
        let mean_passes: Double
        let interception_fraction: Double
        let wall_fraction: Double
        let survivor_fraction: Double
        let effective_transparency: Double
        let max_energy_drift_rel: Double
        let mean_energy_drift_rel: Double
    }
    struct Prediction: Decodable {
        let neutron_rate_n_per_s: Double
        let floor_n_per_s: Double?
        let ceiling_n_per_s: Double
        let cx_included: Bool
        let note: String
    }
    struct ClaimSet: Decodable {
        struct Provenance: Decodable {
            let grid: [Int]
            let ensemble: Int
            let max_passes: Int
            let survivor_fraction: Double
            let max_energy_drift_rel: Double
            let channels: [String]
        }
        let schema: String
        let provenance: Provenance
        let claims: [SolverClaim]
    }
    struct Section: Decodable {
        struct Potential: Decodable {
            let nr: Int, nz: Int
            let dr_mm: Double, dz_mm: Double
            let min_v: Double, max_v: Double
            let values: [Double]
        }
        struct Level: Decodable { let level_v: Double; let polylines: [[[Double]]] }
        struct Wire: Decodable { let r_mm: Double; let z_mm: Double; let radius_mm: Double; let potential_v: Double }
        struct Track: Decodable { let fate: String; let passes: Int; let points: [[Double]] }
        let r_max_mm: Double
        let z_half_mm: Double
        let potential: Potential
        let equipotentials: [Level]
        let wires: [Wire]
        let tracks: [Track]
        let launch_shell_mm: Double
        let core_radius_mm: Double
    }
    struct Sweep: Decodable {
        struct Row: Decodable, Identifiable {
            let kv: Double
            let floor_n_per_s: Double?
            let ceiling_n_per_s: Double
            let interception_fraction: Double
            let mean_passes: Double
            let survivor_fraction: Double
            let max_energy_drift_rel: Double
            var id: Double { kv }
        }
        let grid: [Int]
        let particles: Int
        let rows: [Row]
    }
    struct OperatingPoint: Decodable { let ion_current_a: Double; let d2_pressure_mtorr: Double; let temperature_k: Double }
    struct DeviceInfo: Decodable { let max_potential_drop_v: Double; let rings: Int; let chamber_radius_mm: Double }

    let stats: Stats
    let claim_set: ClaimSet
    let geometric_transparency: Double
    let prediction: Prediction
    let section: Section?
    let sweep: Sweep?
    let operating_point: OperatingPoint
    let device: DeviceInfo

    func claim(_ name: String) -> SolverClaim? { claim_set.claims.first { $0.name == name } }
}

struct OptimizeRun: Decodable {
    struct Eval: Decodable, Identifiable {
        let n: Int
        let start: Int
        let params: [String: Double]
        let neutron_rate_n_per_s: Double
        let best_so_far_n_per_s: Double
        var id: Int { n }
    }
    struct Start: Decodable, Identifiable {
        let start: Int
        let params: [String: Double]
        let neutron_rate_n_per_s: Double
        let history_n_per_s: [Double]
        var id: Int { start }
    }
    let best_params: [String: Double]
    let best_neutron_rate_n_per_s: Double
    let best_start: Int
    let evals: Int
    let starts: [Start]
    let evaluations: [Eval]
    let cancelled: Bool
    let grid: [Int]
    let particles: Int
}

struct ShieldRun: Decodable {
    struct Detector: Decodable, Identifiable {
        let label: String
        let radius_mm: Double
        let dose_usv_per_h: Double
        let rse: Double
        var id: String { label }
    }
    struct Budget: Decodable { let usv_per_h: Double; let pass: Bool }
    struct Layer: Decodable { let material: String; let thickness_mm: Double }
    let detectors: [Detector]
    let bare_detectors: [Detector]?
    let budget: Budget?
    let total_histories: Int
    let source_rate_n_per_s: Double
    let layers: [Layer]
}

struct CompareRun: Decodable {
    struct Row: Decodable, Identifiable {
        let name: String
        let unit: String?
        let predicted: Double
        let measured: Double?
        let ratio: Double?
        let verdict: String
        let acceptance: [Double]?
        let band_factor: Double?
        let uncertainty: Double?
        let instrument: String?
        var id: String { name }
    }
    struct Counts: Decodable { let holds: Int; let violated: Int; let unmeasured: Int }
    let domain: String
    let rows: [Row]
    let counts: Counts
    let measured_verdict: String
    let receipt_verdict: String
}
