import SwiftUI

// The Analysis panel (right rail, ⌥⌘A): DFM, Stress (FEA) and Clearance tabs
// over the open document. State lives in AnalysisStore; the viewport tint
// and clash meshes come from AnalysisOverlay.swift; stress results draw
// through the shared field overlay (FieldOverlay.swift).

struct AnalysisPanel: View {
    @Bindable var model: EditorModel
    @Bindable var store = AnalysisStore.shared

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            PanelHeader(title: "Analysis", systemImage: "gauge.with.dots.needle.33percent",
                        onClose: { store.setPickTarget(nil, model); store.shown = false; store.overlayRevision += 1 })
            Picker("", selection: $store.tab) {
                ForEach(AnalysisTab.allCases) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented).labelsHidden()
            .onChange(of: store.tab) { store.overlayRevision += 1 }
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.Space.m) {
                    switch store.tab {
                    case .dfm: DfmSection(model: model, store: store)
                    case .fea: FeaSection(model: model, store: store)
                    case .clearance: ClearanceSection(model: model, store: store)
                    }
                }
            }
            .frame(maxHeight: 520).fixedSize(horizontal: false, vertical: true)
        }
        .padding(Theme.Space.l).frame(width: Theme.Width.inspector).panelSurface()
        .accessibilityIdentifier("analysis-panel")
    }
}

private func fmt(_ v: Double, _ digits: Int = 2) -> String { String(format: "%.\(digits)f", v) }

private struct StaleNote: View {
    var body: some View {
        Label("The document changed — run again.", systemImage: "arrow.clockwise")
            .font(.caption).foregroundStyle(.orange)
    }
}

// MARK: DFM

private struct DfmSection: View {
    @Bindable var model: EditorModel
    @Bindable var store: AnalysisStore

    var body: some View {
        HStack {
            Picker("Process", selection: $store.dfmProcess) {
                ForEach(AnalysisFFI.dfmProcesses, id: \.self) { Text(label($0)).tag($0) }
            }
            .labelsHidden()
            Button { Task { await store.runDfm(model) } } label: {
                if store.dfmBusy { ProgressView().controlSize(.small) } else { Text("Check") }
            }
            .disabled(store.dfmBusy)
            .keyboardShortcut(.return, modifiers: [.command])
        }
        if let e = store.dfmError { Text(e).font(.caption).foregroundStyle(.red).textSelection(.enabled) }
        if let fix = store.lastFix { Label(fix, systemImage: "checkmark.circle").font(.caption).foregroundStyle(.green) }
        if let r = store.dfmReport {
            if store.dfmStale(model) { StaleNote() }
            HStack(spacing: Theme.Space.m) {
                Text("\(r.score)").font(.title2.monospacedDigit().bold())
                VStack(alignment: .leading) {
                    Text(r.rule_pack_name).font(.caption).lineLimit(1)
                    Text("\(r.counts.error) errors · \(r.counts.warning) warnings · \(r.counts.info) info")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            if r.issues.isEmpty {
                Label("No manufacturability issues.", systemImage: "checkmark.seal").foregroundStyle(.green)
            }
            ForEach(r.issues) { issue in IssueRow(issue: issue, model: model, store: store) }
            if !r.skipped_parts.isEmpty {
                Text("Skipped (no B-rep): \(r.skipped_parts.joined(separator: ", "))").font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    func label(_ p: String) -> String {
        ["fdm": "FDM print", "sla": "Resin (SLA)", "cnc_3axis": "CNC 3-axis", "injection": "Injection molding",
         "sheet_metal": "Sheet metal", "casting_sand": "Sand casting", "casting_investment": "Investment casting"][p] ?? p
    }
}

private struct IssueRow: View {
    let issue: DfmIssue
    @Bindable var model: EditorModel
    @Bindable var store: AnalysisStore
    @State private var expanded = false

    var focused: Bool { store.focusedIssue == issue.id }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline) {
                Image(systemName: issue.severity == "error" ? "xmark.octagon.fill" : issue.severity == "warning" ? "exclamationmark.triangle.fill" : "info.circle")
                    .foregroundStyle(issue.severity == "error" ? Color.red : issue.severity == "warning" ? Color.orange : Color.blue)
                VStack(alignment: .leading, spacing: 2) {
                    Text(issue.ruleTitle).font(.callout.weight(.medium))
                    Text(issue.message).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    Text(issue.part_name).font(.caption2).foregroundStyle(.tertiary)
                }
                Spacer(minLength: 0)
            }
            if expanded, !issue.explanation.isEmpty {
                Text(issue.explanation).font(.caption).fixedSize(horizontal: false, vertical: true)
            }
            if let patch = issue.patch {
                Button { Task { await store.applyFix(issue, model) } } label: {
                    Label(patch.summary, systemImage: "wand.and.stars")
                }
                .controlSize(.small)
                .help("Applies the fix as one undo step, then re-checks.")
            } else if expanded, let note = issue.patch_note {
                Text(note).font(.caption2).foregroundStyle(.secondary)
            }
        }
        .padding(6)
        .selectableRow(selected: focused)
        .contentShape(Rectangle())
        .onTapGesture {
            store.focusedIssue = focused ? nil : issue.id
            expanded = store.focusedIssue == issue.id
            store.overlayRevision += 1
        }
    }
}

// MARK: FEA

private struct FeaSection: View {
    @Bindable var model: EditorModel
    @Bindable var store: AnalysisStore

    var body: some View {
        let partCount = max(1, visibleRoots.count)
        if partCount > 1 {
            Picker("Part", selection: $store.feaPart) {
                ForEach(0..<partCount, id: \.self) { i in
                    Text(partLabel(i)).tag(i)
                }
            }
        }
        Picker("Material", selection: $store.feaMaterial) {
            Text("From part").tag(String?.none)
            ForEach(AnalysisFFI.feaMaterials) { Text($0.label).tag(String?.some($0.id)) }
        }
        Stepper("Mesh: \(store.feaResolution) cells", value: $store.feaResolution, in: 8...48, step: 4)
            .font(.caption)

        Eyebrow("Supports")
        ForEach($store.feaSupports) { $s in
            SetupRow(title: "Fixed", faces: s.faces, picking: store.pickTarget == .support(s.id)) {
                store.setPickTarget(store.pickTarget == .support(s.id) ? nil : .support(s.id), model)
            }
        }
        Eyebrow("Loads")
        ForEach($store.feaLoads) { $l in
            SetupRow(title: "Force", faces: l.faces, picking: store.pickTarget == .load(l.id)) {
                store.setPickTarget(store.pickTarget == .load(l.id) ? nil : .load(l.id), model)
            }
            HStack(spacing: 4) {
                ForEach(0..<3, id: \.self) { a in
                    TextField(["Fx", "Fy", "Fz"][a], value: Binding(get: { l.force[a] }, set: { $l.wrappedValue.force[a] = $0 }),
                              format: .number.precision(.fractionLength(0...2)))
                        .textFieldStyle(.roundedBorder).font(.caption.monospacedDigit())
                }
                Text("N").font(.caption).foregroundStyle(.secondary)
            }
        }
        if store.pickTarget != nil { FacePickList(model: model, store: store) }

        Button { Task { await store.runFea(model) } } label: {
            if store.feaBusy { HStack { ProgressView().controlSize(.small); Text("Solving…") } } else { Text("Solve") }
        }
        .disabled(store.feaBusy)
        if let e = store.feaError { Text(e).font(.caption).foregroundStyle(.red).textSelection(.enabled) }
        if let r = store.feaResult {
            if store.feaStale(model) { StaleNote() }
            Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 3) {
                GridRow { Text("Max von Mises").foregroundStyle(.secondary); Text("\(fmt(r.max_von_mises_mpa)) MPa").monospacedDigit() }
                GridRow { Text("Max displacement").foregroundStyle(.secondary); Text("\(fmt(r.max_displacement_mm, 4)) mm").monospacedDigit() }
                if let y = r.yield_strength_mpa {
                    GridRow { Text("Yield").foregroundStyle(.secondary); Text("\(fmt(y, 0)) MPa · \(r.material)") }
                }
                GridRow {
                    Text("Safety factor").foregroundStyle(.secondary)
                    if let sf = r.safety_factor {
                        Text(fmt(sf)).bold().foregroundStyle(sf >= 1.5 ? Color.green : sf >= 1 ? Color.orange : Color.red)
                    } else {
                        Text("—").foregroundStyle(.secondary)
                    }
                }
            }
            .font(.callout)
            Label(r.converged ? "Mesh-converged" : "Not converged — numbers are indicative",
                  systemImage: r.converged ? "checkmark.seal" : "exclamationmark.triangle")
                .font(.caption).foregroundStyle(r.converged ? Color.green : Color.orange)
            ForEach(r.reasons + r.notes, id: \.self) { Text($0).font(.caption2).foregroundStyle(.secondary) }
            Picker("Show", selection: $store.feaField) {
                ForEach(FeaFieldKind.allCases) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            .onChange(of: store.feaField) { store.showField() }
        }
    }
}

extension FeaSection {
    /// Visible document roots — the evaluated parts, in scene order.
    var visibleRoots: [[String: Any]] {
        ((model.documentJSON?["roots"] as? [[String: Any]]) ?? []).filter { ($0["visible"] as? Bool) != false }
    }
    func partLabel(_ i: Int) -> String {
        let nodes = model.documentJSON?["nodes"] as? [String: Any]
        guard i < visibleRoots.count, let root = visibleRoots[i]["root"] else { return "Part \(i + 1)" }
        return ((nodes?["\(root)"] as? [String: Any])?["name"] as? String) ?? "Part \(i + 1)"
    }
}

private struct SetupRow: View {
    let title: String
    let faces: [UInt32]
    let picking: Bool
    let toggle: () -> Void
    var body: some View {
        HStack {
            Text(title)
            Text(faces.isEmpty ? "no faces" : "\(faces.count) face\(faces.count == 1 ? "" : "s")")
                .foregroundStyle(faces.isEmpty ? AnyShapeStyle(Color.orange) : AnyShapeStyle(.secondary))
            Spacer()
            Button(picking ? "Done" : "Pick Faces", action: toggle).controlSize(.small)
        }
        .font(.callout)
    }
}

/// Face list for the active load/support row (the stub face picker — see
/// AnalysisFacePicking.swift).
private struct FacePickList: View {
    @Bindable var model: EditorModel
    @Bindable var store: AnalysisStore

    var chosen: Set<UInt32> {
        switch store.pickTarget {
        case .load(let id): Set(store.feaLoads.first { $0.id == id }?.faces ?? [])
        case .support(let id): Set(store.feaSupports.first { $0.id == id }?.faces ?? [])
        case nil: []
        }
    }

    var body: some View {
        let faces = store.faces(model)
        VStack(alignment: .leading, spacing: 2) {
            if faces.isEmpty { Text("This part has no pickable faces.").font(.caption).foregroundStyle(.secondary) }
            ForEach(faces) { f in
                HStack {
                    Image(systemName: chosen.contains(f.id) ? "checkmark.square.fill" : "square")
                    Text(f.label).font(.caption)
                    Spacer()
                    Text("\(fmt(f.area, 0)) mm²").font(.caption2).foregroundStyle(.secondary)
                }
                .contentShape(Rectangle())
                .onTapGesture { store.toggle(face: f.id) }
            }
        }
        .padding(6)
        .controlWell()
    }
}

// MARK: Clearance

private struct ClearanceSection: View {
    @Bindable var model: EditorModel
    @Bindable var store: AnalysisStore

    var body: some View {
        Button { Task { await store.runClearance(model) } } label: {
            if store.clearanceBusy { ProgressView().controlSize(.small) } else { Text("Check Clearance") }
        }
        .disabled(store.clearanceBusy)
        if let e = store.clearanceError { Text(e).font(.caption).foregroundStyle(.red) }
        if let r = store.clearance {
            if store.clearanceStale(model) { StaleNote() }
            if r.pairs.isEmpty { Text("Needs two or more parts.").foregroundStyle(.secondary) }
            HStack(spacing: Theme.Space.l) {
                VStack(alignment: .leading) {
                    Text("\(r.interference_count)").font(.title2.bold()).foregroundStyle(r.interference_count > 0 ? Color.red : Color.primary)
                    Text("interfering").font(.caption).foregroundStyle(.secondary)
                }
                if let c = r.min_clearance_mm {
                    VStack(alignment: .leading) {
                        Text("\(fmt(c, 3)) mm").font(.title2.monospacedDigit())
                        Text("min clearance").font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
            ForEach(r.pairs) { p in
                VStack(alignment: .leading, spacing: 2) {
                    Text("\(p.a_name) ↔ \(p.b_name)").font(.callout.weight(.medium)).lineLimit(1)
                    switch p.status {
                    case "interference":
                        Text("Overlap \(fmt(p.overlap_volume_mm3, 2)) mm³" + (p.distance_mm < 0 ? " · depth \(fmt(-p.distance_mm, 3)) mm" : ""))
                            .font(.caption).foregroundStyle(.red)
                    case "contact":
                        Text("Touching").font(.caption).foregroundStyle(.orange)
                    default:
                        Text("Clear by \(fmt(p.distance_mm, 3)) mm").font(.caption).foregroundStyle(.secondary)
                    }
                }
                .padding(.vertical, 2)
            }
        }
    }
}

// MARK: - Menu

struct AnalysisCommands: Commands {
    var body: some Commands {
        CommandMenu("Analyze") {
            Button("Analysis Panel") {
                let s = AnalysisStore.shared
                s.shown.toggle(); s.overlayRevision += 1
            }
            .keyboardShortcut("a", modifiers: [.command, .option])
            Divider()
            ForEach(AnalysisTab.allCases) { tab in
                Button(tab == .dfm ? "Manufacturability (DFM)" : tab == .fea ? "Stress (FEA)" : "Clearance & Interference") {
                    let s = AnalysisStore.shared
                    s.shown = true; s.tab = tab; s.overlayRevision += 1
                }
            }
        }
    }
}
