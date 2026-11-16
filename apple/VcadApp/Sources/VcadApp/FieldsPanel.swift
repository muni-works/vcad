import SwiftUI
import Charts

// Analysis → Fields (AnalysisPanel's fourth tab): the particle-optics,
// optimizer, neutron-dose and bench-receipt screens. Built for the camera as
// much as the desk: hero numbers at title size, every prediction labeled with
// what it is (floor / ceiling / neutron-only), and the bench verdict drawn
// against the exact acceptance window the receipt used.

/// "2.5×10⁴".
func sci(_ v: Double, digits: Int = 2) -> String {
    guard v.isFinite, v != 0 else { return v == 0 ? "0" : "—" }
    let e = Int(floor(log10(abs(v))))
    if (-2...3).contains(e) { return String(format: "%.\(max(0, digits - 1 - max(0, e)))f", v) }
    let m = v / pow(10, Double(e))
    let sup: [Character: Character] = ["0": "⁰", "1": "¹", "2": "²", "3": "³", "4": "⁴", "5": "⁵", "6": "⁶", "7": "⁷", "8": "⁸", "9": "⁹", "-": "⁻"]
    return String(format: "%.\(digits - 1)f", m) + "×10" + String(String(e).map { sup[$0] ?? $0 })
}

struct FieldsTabView: View {
    @Bindable var model: EditorModel
    @Bindable var store = FieldsStore.shared

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            Picker("", selection: $store.tab) {
                ForEach(FieldsTab.allCases) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented).labelsHidden()
            .accessibilityIdentifier("fields-tabs")
            if store.particleBusy || store.optBusy || store.doseBusy {
                RunProgress(store: store)
            }
            switch store.tab {
            case .ions: IonsView(store: store)
            case .optimize: OptimizeView(store: store)
            case .dose: DoseView(store: store)
            case .bench: BenchView(store: store)
            }
        }
    }
}

private struct RunProgress: View {
    @Bindable var store: FieldsStore
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                ProgressView(value: store.progress).progressViewStyle(.linear)
                Button("Cancel") { store.cancel() }.controlSize(.small)
            }
            Text(store.stage).font(.caption.monospacedDigit()).foregroundStyle(.secondary).lineLimit(1)
        }
    }
}

/// A big number with its label and a qualifier underneath.
private struct HeroStat: View {
    let label: String
    let value: String
    var detail: String? = nil
    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Eyebrow(label)
            Text(value).font(.title2.weight(.semibold).monospacedDigit()).lineLimit(1).minimumScaleFactor(0.6)
            if let detail { Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(2) }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Theme.Space.s)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: Theme.Radius.control, style: .continuous))
    }
}

private struct ErrorNote: View {
    let text: String
    var body: some View { Text(text).font(.caption).foregroundStyle(.red).textSelection(.enabled) }
}

// MARK: - Ions

private struct IonsView: View {
    @Bindable var store: FieldsStore

    var body: some View {
        DesignCard(store: store)
        HStack {
            Button { Task { await store.simulate() } } label: {
                Label("Simulate", systemImage: "atom")
            }
            .keyboardShortcut(.return, modifiers: [.command])
            .disabled(store.particleBusy)
            .accessibilityIdentifier("fields-simulate")
            Spacer()
            Toggle("Section", isOn: $store.sectionInPanel).toggleStyle(.checkbox)
                .help("Draw the section in this panel too (it is always on the device in the viewport)")
            Toggle("Tracks", isOn: $store.showTracks).toggleStyle(.checkbox)
                .onChange(of: store.showTracks) { store.overlayRevision += 1 }
            Toggle("In viewport", isOn: $store.showSection).toggleStyle(.checkbox)
                .onChange(of: store.showSection) { AnalysisStore.shared.overlayRevision += 1 }
        }
        if let e = store.particleError { ErrorNote(text: e) }
        if let p = store.particle {
            if let s = p.section, store.sectionInPanel {
                SectionImageView(section: s, style: SectionStyle(heatmap: store.showHeatmap, tracks: store.showTracks))
                    .frame(maxHeight: 420)
            }
            let pr = p.prediction
            HStack(spacing: Theme.Space.s) {
                HeroStat(label: "Predicted yield",
                         value: pr.floor_n_per_s.map { "\(sci($0))–\(sci(pr.ceiling_n_per_s))" } ?? sci(pr.ceiling_n_per_s),
                         detail: pr.cx_included ? "n/s · floor (CX on) – ceiling (CX off)" : "n/s · CX off: a floor on its own physics")
            }
            HStack(spacing: Theme.Space.s) {
                HeroStat(label: "Grid interception", value: String(format: "%.0f%%", 100 * p.stats.interception_fraction),
                         detail: "the cathode-current observable")
                HeroStat(label: "Mean passes", value: String(format: "%.1f", p.stats.mean_passes),
                         detail: String(format: "%.0f%% censored at %d", 100 * p.stats.survivor_fraction, p.claim_set.provenance.max_passes))
                HeroStat(label: "Transparency", value: String(format: "%.2f", p.geometric_transparency),
                         detail: String(format: "geometric · effective %.2f", p.stats.effective_transparency))
            }
            Provenance(run: p, source: store.particleSource)
            if let sw = store.sweep {
                Eyebrow("Voltage sweep")
                YieldChart(rows: sw.rows, currentKv: p.device.max_potential_drop_v / 1000, measured: nil)
                    .frame(height: 210)
                Text("\(sw.grid[0])×\(sw.grid[1]) grid · \(sw.particles) deuterons per point · band = floor…ceiling")
                    .font(.caption).foregroundStyle(.secondary)
            }
        } else if !store.particleBusy {
            EmptyPanelState(title: "No simulation yet", systemImage: "atom",
                            detail: "Solves the fields of the cathode below, traces a deuteron ensemble and predicts the D-D neutron rate.")
        }
    }
}

private struct DesignCard: View {
    @Bindable var store: FieldsStore
    var body: some View {
        let d = store.design
        if !store.designExpanded {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 2) {
                    Eyebrow("Device · classic fusor, axis on Z")
                    Text(String(format: "−%.0f kV · %d-ring grid r %.1f mm · wire r %.1f mm · %.0f mA · %.1f mTorr D₂%@",
                                d.kv, d.rings, d.gridRadiusMm, d.wireRadiusMm, d.ionCurrentMa, d.pressureMtorr, d.cxOn ? " · CX on" : ""))
                        .font(.callout.monospacedDigit())
                }
                Spacer(minLength: 0)
                Button("Edit") { store.designExpanded = true }.controlSize(.small)
            }
        } else {
            form(d)
        }
    }

    @ViewBuilder private func form(_ d: FusorDesign) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Eyebrow("Device · classic fusor, axis on Z")
            Grid(alignment: .leading, horizontalSpacing: Theme.Space.m, verticalSpacing: 4) {
                GridRow {
                    ScrubField(label: "Cathode", value: -d.kv, unit: "kV", sensitivity: 0.5) { v, _ in store.design.kv = min(80, max(1, -v)) }
                    ScrubField(label: "Current", value: d.ionCurrentMa, unit: "mA", sensitivity: 0.1) { v, _ in store.design.ionCurrentMa = max(0.1, v) }
                }
                GridRow {
                    ScrubField(label: "Grid r", value: d.gridRadiusMm, unit: "mm", sensitivity: 0.1) { v, _ in
                        store.design.gridRadiusMm = min(d.chamberRadiusMm - 5, max(d.wireRadiusMm + 1, v)) }
                    ScrubField(label: "Chamber r", value: d.chamberRadiusMm, unit: "mm", sensitivity: 0.5) { v, _ in
                        store.design.chamberRadiusMm = max(d.gridRadiusMm + 5, v) }
                }
                GridRow {
                    ScrubField(label: "Wire r", value: d.wireRadiusMm, unit: "mm", sensitivity: 0.01) { v, _ in
                        store.design.wireRadiusMm = min(5, max(0.1, v)) }
                    ScrubField(label: "D₂", value: d.pressureMtorr, unit: "mTorr", sensitivity: 0.1) { v, _ in store.design.pressureMtorr = max(0.1, v) }
                }
                GridRow {
                    Stepper("Rings: \(d.rings)", value: $store.design.rings, in: 1...12).font(.callout)
                    Toggle("Charge exchange (σ 1e-19 m²)", isOn: $store.design.cxOn).font(.callout)
                }
                GridRow {
                    Picker("Grid", selection: $store.design.gridMm) {
                        Text("1 mm").tag(1.0); Text("0.5 mm").tag(0.5); Text("0.25 mm").tag(0.25)
                    }.font(.callout)
                    Picker("Ions", selection: $store.design.particles) {
                        Text("32").tag(32); Text("64").tag(64); Text("96").tag(96); Text("192").tag(192)
                    }.font(.callout)
                }
            }
        }
    }
}

private struct Provenance: View {
    let run: ParticleRun
    let source: String
    var body: some View {
        let pv = run.claim_set.provenance
        let drift = run.stats.max_energy_drift_rel
        VStack(alignment: .leading, spacing: 2) {
            Text("\(pv.grid[0])×\(pv.grid[1]) grid · \(pv.ensemble) deuterons · \(String(format: "%.0f", run.operating_point.ion_current_a * 1000)) mA · \(String(format: "%.1f", run.operating_point.d2_pressure_mtorr)) mTorr · \(source == "panel" ? "run here" : "run by \(source)")")
            Text("energy drift max \(String(format: "%.0f%%", 100 * drift)) (mean \(String(format: "%.1f%%", 100 * run.stats.mean_energy_drift_rel))) · channels: \(pv.channels.joined(separator: ", "))")
                .foregroundStyle(drift > 0.1 ? Color.orange : Color.secondary)
            Text("Vacuum optics: no space charge, no CX chain — rates are floors on their own physics, not a confidence interval.")
        }
        .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
    }
}

/// Predicted floor…ceiling band vs voltage, optionally with a measured point
/// and the receipt's acceptance window.
struct YieldChart: View {
    let rows: [ParticleRun.Sweep.Row]
    let currentKv: Double
    let measured: (kv: Double, value: Double, sigma: Double, window: [Double]?, holds: Bool?)?

    var body: some View {
        Chart {
            ForEach(rows) { r in
                if let f = r.floor_n_per_s {
                    AreaMark(x: .value("kV", r.kv), yStart: .value("floor", f), yEnd: .value("ceiling", r.ceiling_n_per_s))
                        .foregroundStyle(Color.accentColor.opacity(0.22))
                    LineMark(x: .value("kV", r.kv), y: .value("n/s", f), series: .value("edge", "floor (CX on)"))
                        .foregroundStyle(Color.accentColor)
                        .symbol(.circle)
                }
                LineMark(x: .value("kV", r.kv), y: .value("n/s", r.ceiling_n_per_s), series: .value("edge", "ceiling (CX off)"))
                    .foregroundStyle(Color.accentColor.opacity(0.7))
                    .lineStyle(StrokeStyle(lineWidth: 1.5, dash: [5, 3]))
            }
            RuleMark(x: .value("operating point", currentKv))
                .foregroundStyle(.secondary.opacity(0.5))
                .lineStyle(StrokeStyle(lineWidth: 1, dash: [2, 3]))
            if let m = measured {
                if let w = m.window, w.count == 2 {
                    RuleMark(x: .value("kV", m.kv), yStart: .value("lo", max(w[0], 1)), yEnd: .value("hi", w[1]))
                        .foregroundStyle((m.holds ?? true ? Color.green : Color.red).opacity(0.35))
                        .lineStyle(StrokeStyle(lineWidth: 10, lineCap: .round))
                }
                RuleMark(x: .value("kV", m.kv), yStart: .value("−σ", max(m.value - m.sigma, 1)), yEnd: .value("+σ", m.value + m.sigma))
                    .foregroundStyle(.primary)
                PointMark(x: .value("kV", m.kv), y: .value("measured", m.value))
                    .symbol(.diamond).symbolSize(140)
                    .foregroundStyle(m.holds == false ? Color.red : Color.green)
                    .annotation(position: .trailing) { Text("measured").font(.caption.weight(.semibold)) }
            }
        }
        .chartYScale(type: .log)
        .chartXScale(domain: (rows.map(\.kv).min() ?? 20) - 2 ... (rows.map(\.kv).max() ?? 40) + 2)
        .chartXAxisLabel("cathode voltage, −kV")
        .chartYAxisLabel("D-D neutrons / s")
        .chartYAxis {
            AxisMarks(position: .leading) { v in
                AxisGridLine()
                AxisValueLabel { if let d = v.as(Double.self) { Text(sci(d, digits: 1)) } }
            }
        }
        .chartLegend(position: .top, alignment: .leading)
        .accessibilityIdentifier("fields-yield-chart")
    }
}

// MARK: - Optimize

private struct OptimizeView: View {
    @Bindable var store: FieldsStore
    static let labels = ["grid_r": "Grid radius (mm)", "cathode_v": "Cathode voltage (V)", "wire_a": "Wire radius (mm)"]

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Eyebrow("Search over")
            ForEach(["grid_r", "wire_a", "cathode_v"], id: \.self) { k in
                let r = store.optRanges[k]!
                Toggle(isOn: Binding(get: { store.optVariables.contains(k) },
                                     set: { if $0 { store.optVariables.insert(k) } else if store.optVariables.count > 1 { store.optVariables.remove(k) } })) {
                    HStack {
                        Text(Self.labels[k]!)
                        Spacer()
                        Text("\(sci(r.lowerBound)) … \(sci(r.upperBound))").monospacedDigit().foregroundStyle(.secondary)
                    }
                }
                .font(.callout)
            }
            Text("Objective: predicted D-D yield per ion (CX off), priced in n/s at the device's operating point. Multi-start FD ascent — the landscape is multimodal.")
                .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
        }
        Button { Task { await store.runOptimizer() } } label: { Label("Optimize", systemImage: "chart.line.uptrend.xyaxis") }
            .disabled(store.optBusy)
            .accessibilityIdentifier("fields-optimize")
        if let e = store.optError { ErrorNote(text: e) }

        let points: [(n: Int, start: Int, rate: Double, best: Double)] = store.optimize.map {
            $0.evaluations.map { ($0.n, $0.start, $0.neutron_rate_n_per_s, $0.best_so_far_n_per_s) }
        } ?? store.optLive
        if !points.isEmpty {
            Eyebrow("Every evaluation")
            Chart {
                ForEach(points, id: \.n) { p in
                    PointMark(x: .value("evaluation", p.n), y: .value("n/s", max(p.rate, 1)))
                        .foregroundStyle(by: .value("start", "start \(p.start + 1)"))
                        .symbolSize(28)
                    LineMark(x: .value("evaluation", p.n), y: .value("best", max(p.best, 1)), series: .value("s", "best so far"))
                        .foregroundStyle(.primary)
                        .interpolationMethod(.stepEnd)
                }
            }
            .chartYScale(type: .log)
            .chartXAxisLabel("objective evaluation")
            .chartYAxisLabel("predicted n/s")
            .chartYAxis {
                AxisMarks(position: .leading) { v in
                    AxisGridLine()
                    AxisValueLabel { if let d = v.as(Double.self) { Text(sci(d, digits: 1)) } }
                }
            }
            .frame(height: 220)
            .accessibilityIdentifier("fields-opt-chart")
        }
        if let o = store.optimize {
            HStack(spacing: Theme.Space.s) {
                HeroStat(label: "Best yield", value: "\(sci(o.best_neutron_rate_n_per_s)) n/s",
                         detail: "start \(o.best_start + 1) · \(o.evals) evaluations · \(o.grid[0])×\(o.grid[1]), \(o.particles) ions")
            }
            ForEach(o.best_params.sorted(by: { $0.key < $1.key }), id: \.key) { k, v in
                KeyValueRow(Self.labels[k] ?? k, k == "cathode_v" ? String(format: "%.0f", v) : String(format: "%.2f", v))
            }
            if o.cancelled { Text("Cancelled — best of the evaluations that ran.").font(.caption).foregroundStyle(.orange) }
            Button("Apply to device") {
                if let g = o.best_params["grid_r"] { store.design.gridRadiusMm = g }
                if let w = o.best_params["wire_a"] { store.design.wireRadiusMm = w }
                if let v = o.best_params["cathode_v"] { store.design.kv = -v / 1000 }
                store.tab = .ions
            }
            .controlSize(.small)
            .help("Copy the winning parameters into the Ions design; re-simulate there for full claims.")
        }
    }
}

// MARK: - Dose

private struct DoseView: View {
    @Bindable var store: FieldsStore

    var body: some View {
        let s = store.shield
        VStack(alignment: .leading, spacing: 4) {
            Eyebrow("Shield · spherical, point source at the grid")
            ScrubField(label: "Source", value: s.sourceNPerS / 1e6, unit: "×10⁶ n/s", sensitivity: 0.1) { v, _ in
                store.shield.sourceNPerS = max(1e3, v * 1e6) }
            ScrubField(label: "HDPE", value: s.hdpeMm, unit: "mm", sensitivity: 1) { v, _ in store.shield.hdpeMm = min(600, max(0, v)) }
            ScrubField(label: "Budget", value: s.budgetUsvPerH, unit: "µSv/h", sensitivity: 0.01) { v, _ in
                store.shield.budgetUsvPerH = max(0.01, v) }
            Text("Default source is a 5×10⁶ n/s upper bound (amateur record scale), not the predicted rate: a safety case bounds the machine over-performing.")
                .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
        }
        Button { Task { await store.simulateDose() } } label: { Label("Transport", systemImage: "shield.lefthalf.filled") }
            .disabled(store.doseBusy)
            .accessibilityIdentifier("fields-dose")
        if let e = store.doseError { ErrorNote(text: e) }
        if let d = store.dose {
            if let b = d.budget {
                HStack(spacing: Theme.Space.s) {
                    Image(systemName: b.pass ? "checkmark.seal.fill" : "xmark.octagon.fill")
                        .font(.title).foregroundStyle(b.pass ? .green : .red)
                    VStack(alignment: .leading) {
                        Text(b.pass ? "Within budget" : "Over budget").font(.title2.weight(.semibold))
                        Text(String(format: "every detector ≤ %.2g µSv/h incl. 1σ", b.usv_per_h)).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
            Chart {
                if let bare = d.bare_detectors {
                    ForEach(bare) { x in
                        LineMark(x: .value("m", x.radius_mm / 1000), y: .value("µSv/h", x.dose_usv_per_h), series: .value("s", "bare"))
                            .foregroundStyle(by: .value("s", "no shield"))
                            .lineStyle(StrokeStyle(lineWidth: 1.5, dash: [5, 3]))
                    }
                }
                ForEach(d.detectors) { x in
                    LineMark(x: .value("m", x.radius_mm / 1000), y: .value("µSv/h", x.dose_usv_per_h), series: .value("s", "shielded"))
                        .foregroundStyle(by: .value("s", String(format: "%.0f mm HDPE", d.layers.first { $0.material == "hdpe" }?.thickness_mm ?? 0)))
                        .symbol(.circle)
                    RuleMark(x: .value("m", x.radius_mm / 1000),
                             yStart: .value("lo", x.dose_usv_per_h * (1 - x.rse)), yEnd: .value("hi", x.dose_usv_per_h * (1 + x.rse)))
                        .foregroundStyle(.secondary)
                }
                if let b = d.budget {
                    RuleMark(y: .value("budget", b.usv_per_h))
                        .foregroundStyle(.red)
                        .lineStyle(StrokeStyle(lineWidth: 1.5, dash: [6, 4]))
                        .annotation(position: .top, alignment: .trailing) {
                            Text(String(format: "budget %.2g µSv/h", b.usv_per_h)).font(.caption.weight(.semibold)).foregroundStyle(.red)
                        }
                }
            }
            .chartYScale(type: .log)
            .chartXAxisLabel("distance from source, m")
            .chartYAxisLabel("neutron H*(10), µSv/h")
            .chartForegroundStyleScale(domain: [String(format: "%.0f mm HDPE", d.layers.first { $0.material == "hdpe" }?.thickness_mm ?? 0), "no shield"],
                                       range: [Color.accentColor, Color.secondary])
            .chartLegend(position: .top, alignment: .leading)
            .frame(height: 230)
            .accessibilityIdentifier("fields-dose-chart")
            Grid(alignment: .trailing, horizontalSpacing: Theme.Space.m, verticalSpacing: 2) {
                GridRow {
                    Text("distance").gridColumnAlignment(.leading); Text("no shield"); Text("shielded"); Text("±")
                }.font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                ForEach(Array(d.detectors.enumerated()), id: \.offset) { i, x in
                    GridRow {
                        Text(x.label).gridColumnAlignment(.leading)
                        Text(d.bare_detectors.map { sci($0[i].dose_usv_per_h) } ?? "—")
                        Text(sci(x.dose_usv_per_h)).fontWeight(.semibold)
                            .foregroundStyle(d.budget.map { x.dose_usv_per_h * (1 + x.rse) <= $0.usv_per_h ? Color.primary : Color.red } ?? .primary)
                        Text(String(format: "%.1f%%", 100 * x.rse)).foregroundStyle(.secondary)
                    }
                }
            }
            .font(.callout.monospacedDigit())
            Text("Neutron dose only — no bremsstrahlung x-rays (the dominant hazard at 30–40 kV) and no capture gammas. Spherical geometry, 5-group library ±20–30%. \(sci(Double(d.total_histories))) histories.")
                .font(.caption).foregroundStyle(.orange).fixedSize(horizontal: false, vertical: true)
        } else if !store.doseBusy {
            EmptyPanelState(title: "No dose run yet", systemImage: "shield.lefthalf.filled",
                            detail: "Monte Carlo transport through the HDPE castle; dose vs distance against your budget.")
        }
    }
}

// MARK: - Bench

private struct VerdictChip: View {
    let verdict: String
    var large = false
    var body: some View {
        let (color, icon): (Color, String) = switch verdict.lowercased() {
        case "holds", "pass": (.green, "checkmark.seal.fill")
        case "violated", "fail": (.red, "xmark.octagon.fill")
        case "provisional": (.orange, "hourglass")
        default: (.secondary, "questionmark.circle")
        }
        Label(verdict.uppercased(), systemImage: icon)
            .font(large ? .title.weight(.bold) : .caption.weight(.bold))
            .foregroundStyle(color)
            .padding(.horizontal, large ? 12 : 6).padding(.vertical, large ? 6 : 2)
            .background(color.opacity(0.14), in: Capsule())
    }
}

private struct BenchView: View {
    @Bindable var store: FieldsStore

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Eyebrow("Bench readings")
            ReadingRow(title: "Neutron rate", unit: "n/s", value: $store.bench.neutronRate, sigma: $store.bench.neutronSigma,
                       band: $store.bench.neutronBand,
                       predicted: store.particle.map { "predicted \(sci($0.prediction.neutron_rate_n_per_s)) (floor)" })
            ReadingRow(title: "Interception", unit: "", value: $store.bench.interception, sigma: $store.bench.interceptionSigma,
                       band: $store.bench.interceptionBand,
                       predicted: store.particle.map { String(format: "predicted %.2f", $0.stats.interception_fraction) })
            ReadingRow(title: "Dose @ \(store.bench.doseDetector)", unit: "µSv/h", value: $store.bench.dose, sigma: $store.bench.doseSigma,
                       band: $store.bench.doseBand,
                       predicted: store.dose?.detectors.first { $0.label == store.bench.doseDetector }.map { "predicted \(sci($0.dose_usv_per_h))" })
            if let dets = store.dose?.detectors, !dets.isEmpty {
                Picker("Dose detector", selection: $store.bench.doseDetector) {
                    ForEach(dets) { Text($0.label).tag($0.label) }
                }.font(.callout)
            }
        }
        Button { Task { await store.compare() } } label: { Label("Compare to prediction", systemImage: "checkmark.seal") }
            .keyboardShortcut(.return, modifiers: [.command])
            .accessibilityIdentifier("fields-compare")
        if let e = store.compareError { ErrorNote(text: e) }

        if let c = store.compareParticle {
            VerdictHero(compare: c, title: "Particle optics receipt")
            if let rate = c.rows.first(where: { $0.name == "ddn_neutron_rate" }), let m = rate.measured, let sw = store.sweep,
               let p = store.particle {
                YieldChart(rows: sw.rows, currentKv: p.device.max_potential_drop_v / 1000,
                           measured: (p.device.max_potential_drop_v / 1000, m, rate.uncertainty ?? 0, rate.acceptance, rate.verdict == "Holds"))
                    .frame(height: 220)
                Text("Shaded bar: the acceptance window the verdict used (predicted floor ×/÷ band ± σ). Band: the floor…ceiling bracket.")
                    .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            CompareRows(compare: c)
        }
        if let c = store.compareDose {
            VerdictHero(compare: c, title: "Neutron dose receipt")
            CompareRows(compare: c)
        }
        if store.compareParticle == nil && store.compareDose == nil {
            EmptyPanelState(title: "No measurements bound", systemImage: "gauge.with.needle",
                            detail: "Enter what the bench measured. Each claim comes back Holds, Violated or Unmeasured — unmeasured never passes.")
        }
    }
}

private struct ReadingRow: View {
    let title: String
    let unit: String
    @Binding var value: String
    @Binding var sigma: String
    @Binding var band: String
    let predicted: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Text(title).font(.callout.weight(.medium)).frame(width: 104, alignment: .leading)
                TextField("measured", text: $value).textFieldStyle(.roundedBorder).frame(width: 84)
                Text("±").foregroundStyle(.secondary)
                TextField("σ", text: $sigma).textFieldStyle(.roundedBorder).frame(width: 76)
                Text(unit).font(.caption).foregroundStyle(.secondary).frame(width: 40, alignment: .leading)
                Text("band ×").font(.caption).foregroundStyle(.secondary)
                TextField("2", text: $band).textFieldStyle(.roundedBorder).frame(width: 40)
            }
            .font(.callout.monospacedDigit())
            if let predicted { Text(predicted).font(.caption).foregroundStyle(.secondary).padding(.leading, 110) }
        }
    }
}

private struct VerdictHero: View {
    let compare: CompareRun
    let title: String
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Eyebrow(title)
            HStack(alignment: .center, spacing: Theme.Space.m) {
                VerdictChip(verdict: compare.measured_verdict == "pass" ? "Holds" : compare.measured_verdict == "fail" ? "Violated" : compare.measured_verdict, large: true)
                    .accessibilityIdentifier("fields-verdict")
                VStack(alignment: .leading, spacing: 2) {
                    Text("\(compare.counts.holds) holds · \(compare.counts.violated) violated · \(compare.counts.unmeasured) unmeasured")
                        .font(.callout.monospacedDigit())
                    Text("whole receipt: \(compare.receipt_verdict)\(compare.counts.unmeasured > 0 ? " — unmeasured claims never pass" : "")")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
        }
    }
}

private struct CompareRows: View {
    let compare: CompareRun
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(compare.rows.filter { $0.measured != nil } + compare.rows.filter { $0.measured == nil }) { r in
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    VerdictChip(verdict: r.verdict)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(r.name).font(.callout.weight(.medium))
                        if let m = r.measured {
                            Text("measured \(sci(m)) vs predicted \(sci(r.predicted)) \(r.unit.map { $0 == "1" ? "" : $0 } ?? "") · ratio \(r.ratio.map { String(format: "%.2f", $0) } ?? "—")")
                                .font(.caption.monospacedDigit())
                            if let w = r.acceptance, w.count == 2 {
                                Text("accepted \(sci(w[0]))…\(sci(w[1])) (band ×\(sci(r.band_factor ?? 0)), σ \(sci(r.uncertainty ?? 0)))\(r.instrument.map { " · \($0)" } ?? "")")
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        } else {
                            Text("predicted \(sci(r.predicted)) \(r.unit.map { $0 == "1" ? "" : $0 } ?? "")").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            }
        }
    }
}
