import Foundation
import Observation
import SwiftUI

// One progress system per window. Every long-running operation — opening a
// document, STEP import, assembly meshing, re-solves, slicing, FEA, exports,
// agent jobs — reports into this stack of named tasks instead of drawing its
// own spinner, card or toast. The header title pill (GlassToolbar) is the
// only place that shows it; a details popover lists the tasks when more than
// one is running.
//
// Producers call `begin` / `update` / `end` as often as they like; the
// published `tasks` snapshot changes at most every `interval` (≈10 Hz), so the
// pill's digits stay readable and SwiftUI isn't re-rendered per kernel tick.

/// One running operation.
struct ProgressTask: Identifiable, Equatable {
    /// Stable key chosen by the producer ("load", "solve", "export", …).
    let id: String
    /// What is happening overall: "Importing STEP", "Exporting STL".
    var title: String
    /// The current step within it: "Building faces", "Meshing parts".
    var stage: String?
    /// Counted progress (shown as "339 of 1715"); total 0 = uncounted.
    var done: Int = 0
    var total: Int = 0
    /// Explicit fraction for producers that have one but no counts.
    var explicitFraction: Double?
    let started: Date
    /// Non-nil when the task can be stopped. Every cancel goes through
    /// `ProgressCenter.cancel(_:)`.
    var cancellable: Bool

    /// 0…1, or nil when nothing is measurable (the bar shimmers).
    var fraction: Double? {
        if let f = explicitFraction { return min(1, max(0, f)) }
        guard total > 1 else { return nil }
        return min(1, max(0, Double(done) / Double(total)))
    }

    /// "339 of 1715" when counted, "12%" with only a fraction, else nil.
    var amountLabel: String? {
        if total > 1 { return "\(min(done, total)) of \(total)" }
        return percentLabel
    }

    /// Seconds left, once there is enough history to say (≥10 % done and
    /// ≥3 s in). Nil otherwise.
    func secondsRemaining(now: Date) -> Double? {
        guard let f = fraction, f >= 0.1, f < 1 else { return nil }
        let elapsed = now.timeIntervalSince(started)
        guard elapsed >= 3 else { return nil }
        return elapsed * (1 - f) / f
    }

    /// "~40 s left" / "~2 min left".
    func etaLabel(now: Date) -> String? {
        guard let s = secondsRemaining(now: now) else { return nil }
        if s < 60 { return "~\(max(1, Int(s.rounded()))) s left" }
        return "~\(Int((s / 60).rounded())) min left"
    }

    /// Whole percent, when measurable: "12%".
    var percentLabel: String? { fraction.map { "\(Int(($0 * 100).rounded(.down)))%" } }

    /// The pill's second line, short enough for a narrow pill:
    /// "Importing STEP · Building faces · 12%". Counts and ETA live in
    /// `detailLine` (popover, tooltip).
    var line: String {
        [title, stage, percentLabel].compactMap { $0 }.joined(separator: " · ")
    }

    /// "339 of 1715 · ~40 s left".
    func detailLine(now: Date) -> String? {
        let parts = [amountLabel, etaLabel(now: now)].compactMap { $0 }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    static func == (a: ProgressTask, b: ProgressTask) -> Bool {
        a.id == b.id && a.title == b.title && a.stage == b.stage && a.done == b.done
            && a.total == b.total && a.explicitFraction == b.explicitFraction
            && a.started == b.started && a.cancellable == b.cancellable
    }
}

@MainActor
@Observable
final class ProgressCenter {
    /// What the UI draws: newest-started last. Throttled.
    private(set) var tasks: [ProgressTask] = []

    /// Minimum time between published snapshots.
    @ObservationIgnored let interval: TimeInterval
    @ObservationIgnored private let now: () -> Date
    @ObservationIgnored private var pending: [ProgressTask] = []
    @ObservationIgnored private var cancels: [String: () -> Void] = [:]
    @ObservationIgnored private var lastPublish: Date = .distantPast
    @ObservationIgnored private var flushScheduled = false
    /// Tests drive flushes by hand.
    @ObservationIgnored var schedulesFlush = true

    init(interval: TimeInterval = 0.1, now: @escaping () -> Date = Date.init) {
        self.interval = interval
        self.now = now
    }

    /// The task the pill headlines: the most recently started.
    var primary: ProgressTask? { tasks.last }

    /// Start (or restart) a task. A begin with an existing id replaces it.
    func begin(_ id: String, title: String, stage: String? = nil,
               done: Int = 0, total: Int = 0, fraction: Double? = nil,
               cancel: (() -> Void)? = nil) {
        let task = ProgressTask(id: id, title: title, stage: stage, done: done, total: total,
                                explicitFraction: fraction, started: now(), cancellable: cancel != nil)
        pending.removeAll { $0.id == id }
        pending.append(task)
        cancels[id] = cancel
        // Starting or finishing a task is news: show it now, not 100 ms late.
        publish()
    }

    /// Report progress. Unknown ids are ignored (the task already ended).
    func update(_ id: String, title: String? = nil, stage: String?? = nil,
                done: Int? = nil, total: Int? = nil, fraction: Double?? = nil) {
        guard let i = pending.firstIndex(where: { $0.id == id }) else { return }
        var t = pending[i]
        if let title { t.title = title }
        if let stage { t.stage = stage }
        if let done { t.done = done }
        if let total { t.total = total }
        if let fraction { t.explicitFraction = fraction }
        guard t != pending[i] else { return }
        pending[i] = t
        requestPublish()
    }

    /// Start the task if it isn't running, else update it.
    func report(_ id: String, title: String, stage: String? = nil,
                done: Int = 0, total: Int = 0, fraction: Double? = nil,
                cancel: (() -> Void)? = nil) {
        if pending.contains(where: { $0.id == id }) {
            update(id, title: title, stage: .some(stage), done: done, total: total, fraction: .some(fraction))
            cancels[id] = cancel
            if let i = pending.firstIndex(where: { $0.id == id }), pending[i].cancellable != (cancel != nil) {
                pending[i].cancellable = cancel != nil
                publish()
            }
        } else {
            begin(id, title: title, stage: stage, done: done, total: total, fraction: fraction, cancel: cancel)
        }
    }

    func end(_ id: String) {
        guard pending.contains(where: { $0.id == id }) else { return }
        pending.removeAll { $0.id == id }
        cancels[id] = nil
        publish()
    }

    func isRunning(_ id: String) -> Bool { pending.contains { $0.id == id } }

    /// The one cancel path for every task. Ends the task, then runs the
    /// producer's cancel.
    func cancel(_ id: String) {
        guard let action = cancels[id] else { return }
        end(id)
        action()
    }

    /// Publish whatever is pending now (tests; also the scheduled flush).
    func flush() {
        flushScheduled = false
        publish()
    }

    private func requestPublish() {
        let wait = interval - now().timeIntervalSince(lastPublish)
        if wait <= 0 { publish(); return }
        guard schedulesFlush, !flushScheduled else { return }
        flushScheduled = true
        Task { @MainActor [weak self] in
            try? await Task.sleep(for: .seconds(wait))
            self?.flush()
        }
    }

    private func publish() {
        lastPublish = now()
        if tasks != pending { tasks = pending }
    }
}

// MARK: - Producers bound to a busy flag

extension View {
    /// Report a task to the window's progress center while `active` is true:
    /// for panels whose work is a busy flag (FEA, slicing, DFM, exports).
    func reportsProgress(_ center: ProgressCenter, id: String, title: String, active: Bool,
                         stage: String? = nil, fraction: Double? = nil,
                         cancel: (() -> Void)? = nil) -> some View {
        modifier(ReportsProgress(center: center, id: id, title: title, active: active,
                                 stage: stage, fraction: fraction, cancel: cancel))
    }
}

private struct ReportsProgress: ViewModifier {
    let center: ProgressCenter
    let id: String
    let title: String
    let active: Bool
    let stage: String?
    let fraction: Double?
    let cancel: (() -> Void)?

    func body(content: Content) -> some View {
        content
            .onAppear { sync() }
            .onChange(of: active) { sync() }
            .onChange(of: stage) { sync() }
            .onChange(of: fraction) { sync() }
            .onDisappear { if active { center.end(id) } }
    }

    private func sync() {
        if active {
            center.report(id, title: title, stage: stage, fraction: fraction, cancel: cancel)
        } else {
            center.end(id)
        }
    }
}

// MARK: - Views (drawn by GlassToolbar's title pill)

/// The thin bar along the pill's bottom edge: determinate when it can be, a
/// soft sweep only when nothing is measurable.
struct PillProgressBar: View {
    let fraction: Double?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var sweep: CGFloat = -0.3

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                if let f = fraction {
                    Capsule().fill(Color.accentColor)
                        .frame(width: max(4, geo.size.width * f))
                        .animation(.smooth(duration: 0.3), value: f)
                } else {
                    let band = max(30, geo.size.width * 0.3)
                    Capsule()
                        .fill(LinearGradient(colors: [.accentColor.opacity(0), .accentColor, .accentColor.opacity(0)],
                                             startPoint: .leading, endPoint: .trailing))
                        .frame(width: band)
                        .offset(x: reduceMotion ? (geo.size.width - band) / 2 : sweep * geo.size.width)
                        .onAppear {
                            guard !reduceMotion else { return }
                            withAnimation(.easeInOut(duration: 1.3).repeatForever(autoreverses: false)) { sweep = 1 }
                        }
                }
            }
            .frame(maxHeight: .infinity, alignment: .bottom)
        }
        .frame(height: 2)
        .clipShape(Capsule())
        .accessibilityElement()
        .accessibilityLabel("Progress")
        .accessibilityValue(fraction.map { "\(Int($0 * 100)) percent" } ?? "In progress")
    }
}

/// The pill's popover: every running task (each with the same ✕), and a
/// failed or warned open with its details and Retry / Close / OK.
struct ProgressDetails: View {
    @Bindable var model: EditorModel
    let close: () -> Void
    @State private var showsDetails = false

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            ForEach(model.progress.tasks) { task in
                TimelineView(.periodic(from: .now, by: 1)) { ctx in
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(alignment: .firstTextBaseline) {
                            Text(task.title).font(.callout.weight(.medium))
                            Spacer(minLength: Theme.Space.s)
                            Text(task.detailLine(now: ctx.date) ?? "")
                                .font(.caption).foregroundStyle(.secondary)
                                .monospacedDigit().contentTransition(.identity)
                            if task.cancellable {
                                Button { model.progress.cancel(task.id) } label: {
                                    Image(systemName: "xmark.circle.fill").symbolRenderingMode(.hierarchical)
                                }
                                .buttonStyle(.plain).foregroundStyle(.secondary)
                                .help("Cancel \(task.title.lowercased())")
                                .accessibilityLabel("Cancel \(task.title)")
                            }
                        }
                        if let stage = task.stage {
                            Text(stage).font(.caption).foregroundStyle(.secondary)
                        }
                        PillProgressBar(fraction: task.fraction).frame(height: 3)
                    }
                }
            }
            if let load = model.loading, !load.isRunning { outcome(load) }
        }
        .padding(Theme.Space.l)
        .frame(width: 320)
    }

    @ViewBuilder private func outcome(_ load: LoadProgress) -> some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            Label {
                Text(load.stageLabel).font(.callout)
                    .fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
            } icon: {
                Image(systemName: load.isFailed ? "exclamationmark.octagon.fill" : "exclamationmark.triangle.fill")
                    .foregroundStyle(load.isFailed ? .red : .orange)
            }
            Text(load.fileName).font(.caption).foregroundStyle(.secondary)
            if showsDetails, let d = load.details {
                ScrollView {
                    Text(d).font(.caption.monospaced()).foregroundStyle(.secondary)
                        .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 140)
            }
            HStack {
                if load.details != nil {
                    Button(showsDetails ? "Hide Details" : "Show Details") { showsDetails.toggle() }
                        .buttonStyle(.link).font(.callout)
                }
                Spacer()
                if load.isFailed {
                    Button("Close") { model.dismissLoading(); close() }
                    Button("Retry") { close(); model.retryLoading() }.keyboardShortcut(.defaultAction)
                } else {
                    Button("OK") { model.dismissLoading(); close() }.keyboardShortcut(.defaultAction)
                }
            }
        }
    }
}
