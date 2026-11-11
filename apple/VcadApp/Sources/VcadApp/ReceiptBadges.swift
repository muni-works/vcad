import SwiftUI

// Live receipt badges (M2): one capsule per persisted assertion, top of the
// viewport. Holds is green, Stale gray, Violated red; they re-verify on every
// edit (ReceiptStore), so a nudge that breaks a clearance turns its badge red
// at once. Click a badge for the measurement, why it isn't green, and Accept
// (re-baseline a Stale assertion — a Violated one can't be accepted).

extension ReceiptStatus {
    var color: Color {
        switch self { case .holds: .green; case .stale: .gray; case .violated: .red }
    }
    var symbol: String {
        switch self {
        case .holds: "checkmark.seal.fill"
        case .stale: "clock.badge.questionmark"
        case .violated: "xmark.octagon.fill"
        }
    }
}

struct ReceiptBadgeStrip: View {
    let model: EditorModel
    @Bindable var store = ReceiptStore.shared

    var body: some View {
        if let report = store.report, !report.checks.isEmpty {
            HStack(spacing: Theme.Space.s) {
                ForEach(report.checks) { c in
                    ReceiptBadge(check: c, verifying: store.verifying, model: model)
                }
                if store.verifying {
                    ProgressView().controlSize(.small).help("Re-verifying receipts…")
                }
            }
            .animation(Motion.snappy, value: report.checks.map(\.status))
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Receipts")
        }
    }
}

struct ReceiptBadge: View {
    let check: ReceiptCheck
    let verifying: Bool
    let model: EditorModel
    @Bindable var store = ReceiptStore.shared
    @State private var accepting = false

    private var shown: Binding<Bool> {
        Binding(get: { store.expanded == check.id }, set: { store.expanded = $0 ? check.id : nil })
    }

    var body: some View {
        Button { shown.wrappedValue.toggle() } label: {
            HStack(spacing: 6) {
                Image(systemName: check.status.symbol)
                Text(check.label).fontWeight(.medium)
                Text(check.summary).foregroundStyle(.secondary).monospacedDigit()
            }
            .font(.callout)
            .foregroundStyle(check.status == .stale ? Color.secondary : check.status.color)
            .padding(.horizontal, 12).padding(.vertical, 6)
            .pillSurface()
            .overlay(Capsule(style: .continuous)
                .strokeBorder(check.status.color.opacity(check.status == .stale ? 0.4 : 0.6), lineWidth: 1.5))
            .opacity(verifying ? 0.7 : 1)
        }
        .buttonStyle(.plain)
        .help("\(check.claimId): \(check.status.rawValue)")
        .accessibilityLabel("Receipt \(check.label), \(check.status.rawValue), \(check.summary)")
        .accessibilityIdentifier("receipt-badge-\(check.label)")
        .popover(isPresented: shown, arrowEdge: .bottom) { detail }
    }

    private var detail: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            Label(check.status.rawValue, systemImage: check.status.symbol)
                .font(.headline).foregroundStyle(check.status.color)
            Text(check.subject).font(.callout)
            Grid(alignment: .leading, horizontalSpacing: Theme.Space.m, verticalSpacing: 4) {
                row("Required", "≥ \(ReceiptCheck.mm(check.requiredMm))")
                if let m = check.measuredMm { row("Measured", String(format: "%.3f mm", m)) }
                if let b = check.baselineMm { row("Accepted", String(format: "%.3f mm", b)) }
                if let p = check.worstPair, p.count == 2 { row("Closest", "\(p[0]) ↔ \(p[1])") }
            }
            .font(.callout.monospacedDigit())
            if let r = check.reason { Text(r).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true) }
            Text(check.claimId).font(.caption2.monospaced()).foregroundStyle(.tertiary)
            if check.status != .violated && (check.baselineMm == nil || check.status == .stale) {
                Button(accepting ? "Accepting…" : "Accept measurement") {
                    accepting = true
                    Task { _ = try? await store.accept(model, label: check.label); accepting = false }
                }
                .disabled(accepting)
            }
        }
        .padding(14)
        .frame(width: 280, alignment: .leading)
    }

    private func row(_ k: String, _ v: String) -> some View {
        GridRow {
            Text(k).foregroundStyle(.secondary)
            Text(v)
        }
    }
}

/// The viewport's top-centre stack: receipt badges over any live pick
/// elicitation, plus the form-elicitation sheet. Feeds document changes to
/// the receipt store, so badges re-verify on every edit.
struct ProofOverlay: View {
    let model: EditorModel
    @Bindable var elicit = ElicitationCenter.shared

    var body: some View {
        VStack(spacing: Theme.Space.s) {
            ReceiptBadgeStrip(model: model)
            ElicitationBanner()
        }
        .task(id: model.documentRevision) { ReceiptStore.shared.documentChanged(model.documentJSON) }
        .onChange(of: model.selectedPartIndex) { _, i in
            elicit.partSelectionChanged(i.map { [$0] } ?? [])
        }
        // Dismissed only by its own buttons (Esc is its Cancel), so a queued
        // request is never cancelled by the previous sheet closing.
        .sheet(isPresented: Binding(get: { elicit.showsSheet }, set: { _ in })) {
            ElicitationSheet().interactiveDismissDisabled()
        }
    }
}
