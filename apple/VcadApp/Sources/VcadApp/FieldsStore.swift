import Foundation
import SwiftUI

// Analysis → Fields: the particle-optics, electrode-optimizer, neutron-dose
// and bench-comparison screens (the open-fusor video's sim shots). State and
// actions live here; the views are FieldsPanel.swift and the viewport
// section overlay is FieldsSection.swift.
//
// Every number comes from `SolverFFI` — the same vcad-solver-tools code the
// agent's tools run. When the agent runs one of those tools in this window,
// the MCP core hands the full result over (`__show_solver_result`, see
// AgentCore) and `ingest` puts it on screen, so the panel shows what the
// agent saw.

enum FieldsTab: String, CaseIterable, Identifiable {
    case ions = "Ions", optimize = "Optimize", dose = "Dose", bench = "Bench"
    var id: String { rawValue }
}

/// The hobby fusor of docs/videos/open-fusor.md (6" chamber, 1.5" grid as a
/// 4-ring latitude stack of 2 mm wire, −30 kV, 10 mA, 5 mTorr D₂).
struct FusorDesign: Equatable {
    var chamberRadiusMm = 76.0
    var gridRadiusMm = 19.0
    var rings = 4
    var wireRadiusMm = 1.0
    var kv = 30.0
    var ionCurrentMa = 10.0
    var pressureMtorr = 5.0
    var cxOn = true
    var cxSigmaM2 = 1e-19
    /// Grid spacing, mm (the solve's h). 0.25 mm is the plan's finest row:
    /// coarser grids fatten the wire mask and read interception ≈ 1.00.
    var gridMm = 0.25
    var particles = 96
    var maxPasses = 40
    var sweepKv: [Double] = [20, 25, 30, 35, 40]

    var nr: Int { Int((chamberRadiusMm / gridMm).rounded()) + 1 }
    var nz: Int { Int((2 * chamberRadiusMm / gridMm).rounded()) + 1 }

    /// `classic_fusor` shorthand; `named` turns fields into parameters.
    func spec(named: Set<String> = []) -> [String: Any] {
        func v(_ key: String, _ value: Double) -> Any { named.contains(key) ? key : value }
        return ["classic_fusor": [
            "chamber_radius_mm": chamberRadiusMm,
            "cathode_radius_mm": v("grid_r", gridRadiusMm),
            "rings": rings,
            "wire_radius_mm": v("wire_a", wireRadiusMm),
            "cathode_v": v("cathode_v", -kv * 1000),
        ] as [String: Any]]
    }

    var options: [String: Any] {
        var o: [String: Any] = ["nr": nr, "nz": nz, "particles": particles, "max_passes": maxPasses,
                                "ion_current_a": ionCurrentMa / 1000, "d2_pressure_mtorr": pressureMtorr]
        if cxOn { o["cx_sigma_m2"] = cxSigmaM2 }
        return o
    }

    func simulateArgs(sweep: Bool) -> [String: Any] {
        var a: [String: Any] = ["spec": spec(), "options": options,
                                "section": ["tracks": 28, "contours": 16, "max_points": 900, "spacing_mm": 0.3]]
        if sweep {
            // The sweep runs a notch coarser than the operating point.
            let h = max(gridMm, 0.5)
            a["sweep"] = ["kv": sweepKv, "nr": Int((chamberRadiusMm / h).rounded()) + 1,
                          "nz": Int((2 * chamberRadiusMm / h).rounded()) + 1, "particles": max(32, particles / 2)]
        }
        return a
    }
}

/// The shield problem: chamber gap, HDPE castle, room air, detectors.
struct ShieldDesign: Equatable {
    var sourceNPerS = 5e6
    var innerGapMm = 100.0
    var hdpeMm = 150.0
    var roomMm = 3200.0
    var detectorsM: [Double] = [0.5, 0.75, 1, 1.5, 2, 2.5, 3]
    var budgetUsvPerH = 1.0
    var historiesPerBatch = 20_000
    var batches = 20

    func label(_ m: Double) -> String { m == m.rounded() ? "\(Int(m)) m" : String(format: "%.2g m", m) }

    var args: [String: Any] {
        var layers: [[String: Any]] = [["material": "air", "thickness_mm": innerGapMm]]
        if hdpeMm > 0 { layers.append(["material": "hdpe", "thickness_mm": hdpeMm]) }
        layers.append(["material": "air", "thickness_mm": roomMm])
        return [
            "spec": [
                "layers": layers,
                "source": ["rate_n_per_s": sourceNPerS, "energy_ev": 2.45e6],
                "detectors": detectorsM.map { ["label": label($0), "radius_mm": $0 * 1000] },
                "run": ["histories_per_batch": historiesPerBatch, "batches": batches, "seed": 20260717],
            ] as [String: Any],
            "budget_usv_per_h": budgetUsvPerH,
            "compare_bare": true,
        ]
    }
}

/// Bench readings for the receipt.
struct BenchReadings: Equatable {
    var neutronRate = ""         // n/s
    var neutronSigma = ""
    var neutronBand = "3"
    var neutronInstrument = "moderated He-3 tube"
    var interception = ""        // 0…1
    var interceptionSigma = "0.03"
    var interceptionBand = "1.25"
    var dose = ""                // µSv/h
    var doseSigma = ""
    var doseBand = "1.5"
    var doseDetector = "1 m"
    var doseInstrument = "neutron survey meter (rem ball)"
}

@MainActor @Observable
final class FieldsStore {
    static let shared = FieldsStore()

    var tab: FieldsTab = .ions
    /// The device form; collapses to a one-line summary (more room for results).
    var designExpanded = true
    var design = FusorDesign()
    var shield = ShieldDesign()
    var bench = BenchReadings()

    // Ions
    var particle: ParticleRun?
    /// Raw `claim_set` of `particle` (passed back to compare_measurements unchanged).
    @ObservationIgnored var particleClaimSet: Any?
    var particleBusy = false
    var particleError: String?
    /// Who ran it: "panel" or the agent's display name.
    var particleSource = "panel"
    /// The form the panel's last run used (nil for an agent's run, whose spec is its own).
    var particleDesign: FusorDesign?

    // Optimize
    var optVariables: Set<String> = ["grid_r"]
    var optRanges: [String: ClosedRange<Double>] = ["grid_r": 10...40, "cathode_v": -40_000 ... -20_000, "wire_a": 0.5...2.0]
    var optimize: OptimizeRun?
    var optBusy = false
    var optError: String?
    /// Live evaluations parsed from progress while the run is going.
    var optLive: [(n: Int, start: Int, rate: Double, best: Double)] = []

    // Dose
    var dose: ShieldRun?
    @ObservationIgnored var doseClaimSet: Any?
    var doseBusy = false
    var doseError: String?

    // Bench
    var compareParticle: CompareRun?
    var compareDose: CompareRun?
    var compareError: String?

    // Progress (one at a time is enough for a panel)
    var progress = 0.0
    var stage = ""

    /// Draw the section in the viewport while the Fields tab is up.
    var showSection = true
    var showTracks = true
    var showHeatmap = true
    /// Also draw the section in the panel (off: the viewport has it; room for the sweep).
    var sectionInPanel = true
    /// Bumped on anything the viewport section draws.
    var overlayRevision = 0

    func cancel() { Self.setCancel(true) }

    private func json(_ obj: Any) throws -> Data { try JSONSerialization.data(withJSONObject: obj, options: [.sortedKeys]) }

    /// Run a solver tool off main with progress into `progress` / `stage`.
    private func run(_ name: String, _ args: [String: Any], onStep: (@MainActor (Double, String) -> Void)? = nil) async -> Result<Data, Error> {
        progress = 0; stage = "starting"
        let body: Data
        do { body = try json(args) } catch { return .failure(error) }
        return await Task.detached(priority: .userInitiated) {
            Result {
                try SolverFFI.run(name, args: body) { f, msg in
                    Task { @MainActor in
                        let s = FieldsStore.shared
                        s.progress = f; s.stage = msg
                        onStep?(f, msg)
                    }
                    return !FieldsStore.cancelRequested()
                }
            }
        }.value
    }

    nonisolated private static let cancelLock = NSLock()
    nonisolated(unsafe) private static var _cancel = false
    nonisolated static func cancelRequested() -> Bool { cancelLock.lock(); defer { cancelLock.unlock() }; return _cancel }
    nonisolated static func setCancel(_ v: Bool) { cancelLock.lock(); _cancel = v; cancelLock.unlock() }

    // MARK: Ions

    func simulate(sweep: Bool = true) async {
        particleBusy = true; particleError = nil
        Self.setCancel(false)
        let used = design
        let r = await run("simulate_charged_particles", used.simulateArgs(sweep: sweep))
        particleBusy = false
        switch r {
        case .success(let d): ingestParticle(d, source: "panel"); particleDesign = used
        case .failure(let e): particleError = "\(e)"
        }
    }

    func ingestParticle(_ d: Data, source: String) {
        do {
            let run = try JSONDecoder().decode(ParticleRun.self, from: d)
            // Keep a previous sweep when this run has none (an agent's single point).
            let keep = run.sweep == nil ? particle?.sweep : nil
            particle = run
            if let keep, run.sweep == nil { particleSweepCarry = keep } else { particleSweepCarry = nil }
            particleClaimSet = (try JSONSerialization.jsonObject(with: d) as? [String: Any])?["claim_set"]
            particleSource = source
            compareParticle = nil
            overlayRevision += 1
        } catch {
            particleError = "could not read the result: \(error)"
        }
    }

    /// A sweep kept from an earlier run when the latest run had none.
    var particleSweepCarry: ParticleRun.Sweep?
    var sweep: ParticleRun.Sweep? { particle?.sweep ?? particleSweepCarry }

    // MARK: Optimize

    func runOptimizer() async {
        optBusy = true; optError = nil; optLive = []
        Self.setCancel(false)
        let vars = optVariables.sorted()
        let d = design
        var params: [String: Double] = ["grid_r": d.gridRadiusMm, "wire_a": d.wireRadiusMm, "cathode_v": -d.kv * 1000]
        params = params.filter { vars.contains($0.key) }
        let args: [String: Any] = [
            "spec": d.spec(named: Set(vars)),
            "parameters": params,
            "variables": vars.map { ["name": $0, "lo": optRanges[$0]!.lowerBound, "hi": optRanges[$0]!.upperBound] },
            "options": ["nr": 61, "nz": 121, "particles": 32, "max_passes": 20, "max_iters": 6, "multi_start": true,
                        "ion_current_a": d.ionCurrentMa / 1000, "d2_pressure_mtorr": d.pressureMtorr],
        ]
        let r = await run("optimize_electrodes", args) { _, msg in
            if let e = Self.parseEval(msg) { FieldsStore.shared.optLive.append(e) }
        }
        optBusy = false
        switch r {
        case .success(let data): ingestOptimize(data)
        case .failure(let e): optError = "\(e)"
        }
    }

    func ingestOptimize(_ d: Data) {
        do { optimize = try JSONDecoder().decode(OptimizeRun.self, from: d) } catch { optError = "could not read the result: \(error)" }
    }

    /// "eval 12 · start 2/3 · grid_r=19.0000 → 3.200e4 n/s (best 4.100e4)".
    nonisolated static func parseEval(_ s: String) -> (n: Int, start: Int, rate: Double, best: Double)? {
        guard s.hasPrefix("eval ") else { return nil }
        let parts = s.components(separatedBy: " · ")
        guard parts.count >= 3, let n = Int(parts[0].dropFirst(5)),
              let st = Int(parts[1].replacingOccurrences(of: "start ", with: "").split(separator: "/").first ?? ""),
              let arrow = s.range(of: "→ ") else { return nil }
        let tail = s[arrow.upperBound...]
        let nums = tail.split(whereSeparator: { " ()".contains($0) })
        guard nums.count >= 4, let rate = Double(nums[0]), let best = Double(nums[3]) else { return nil }
        return (n, st - 1, rate, best)
    }

    // MARK: Dose

    func simulateDose() async {
        doseBusy = true; doseError = nil
        Self.setCancel(false)
        let r = await run("simulate_neutron_shield", shield.args)
        doseBusy = false
        switch r {
        case .success(let d): ingestDose(d)
        case .failure(let e): doseError = "\(e)"
        }
    }

    func ingestDose(_ d: Data) {
        do {
            dose = try JSONDecoder().decode(ShieldRun.self, from: d)
            doseClaimSet = (try JSONSerialization.jsonObject(with: d) as? [String: Any])?["claim_set"]
            compareDose = nil
        } catch { doseError = "could not read the result: \(error)" }
    }

    // MARK: Bench

    private func num(_ s: String) -> Double? { Double(s.replacingOccurrences(of: ",", with: "").trimmingCharacters(in: .whitespaces)) }

    func compare() async {
        compareError = nil
        let b = bench
        var pm: [[String: Any]] = []
        if let v = num(b.neutronRate) {
            pm.append(["name": "ddn_neutron_rate", "value": v, "uncertainty": num(b.neutronSigma) ?? 0,
                       "band_factor": num(b.neutronBand) ?? 2, "instrument": b.neutronInstrument])
        }
        if let v = num(b.interception) {
            pm.append(["name": "interception_fraction", "value": v, "uncertainty": num(b.interceptionSigma) ?? 0,
                       "band_factor": num(b.interceptionBand) ?? 1.25, "instrument": "cathode ammeter / supply current"])
        }
        if let cs = particleClaimSet, !pm.isEmpty {
            switch await run("compare_measurements", ["claim_set": cs, "measurements": pm]) {
            case .success(let d): compareParticle = try? JSONDecoder().decode(CompareRun.self, from: d)
            case .failure(let e): compareError = "\(e)"
            }
        } else if !pm.isEmpty { compareError = "Run Ions first — there is no prediction to compare against." }
        if let v = num(b.dose) {
            let m: [String: Any] = ["name": "dose_rate:\(b.doseDetector)", "value": v, "uncertainty": num(b.doseSigma) ?? 0,
                                    "band_factor": num(b.doseBand) ?? 1.5, "instrument": b.doseInstrument]
            if let cs = doseClaimSet {
                switch await run("compare_measurements", ["claim_set": cs, "measurements": [m]]) {
                case .success(let d): compareDose = try? JSONDecoder().decode(CompareRun.self, from: d)
                case .failure(let e): compareError = "\(e)"
                }
            } else { compareError = "Run Dose first — there is no dose prediction to compare against." }
        }
        progress = 1; stage = "compared"
    }

    // MARK: Agent results (AgentCore `__show_solver_result`)

    /// The full result of a solver tool the agent ran in this window.
    func ingest(tool: String, result: Any, agent: String) {
        guard let d = try? JSONSerialization.data(withJSONObject: result) else { return }
        AnalysisStore.shared.shown = true
        AnalysisStore.shared.tab = .fields
        switch tool {
        case "simulate_charged_particles": tab = .ions; ingestParticle(d, source: agent); particleDesign = nil
        case "optimize_electrodes": tab = .optimize; ingestOptimize(d)
        case "simulate_neutron_shield": tab = .dose; ingestDose(d)
        case "compare_measurements":
            tab = .bench
            if let c = try? JSONDecoder().decode(CompareRun.self, from: d) {
                if c.domain == "neutronics" { compareDose = c } else { compareParticle = c }
            }
        default: return
        }
        AnalysisStore.shared.overlayRevision += 1
    }
}
