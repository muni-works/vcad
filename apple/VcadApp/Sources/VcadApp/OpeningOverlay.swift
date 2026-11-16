import SwiftUI

// What the viewport shows while an opened document's first solve runs.
//
// The header pill alone ("Opening · Evaluating" and a sliver of bar) read as
// a frozen window: the model tree and inspector were already filled in from
// the parsed document, but the stage stayed empty. This overlay says which
// part is solving, how many have landed, and for how long, with a strip of one
// segment per part that fills in as parts land. It is centred until the first
// part is on screen, then folds into a small chip above the dock so it never
// hides the parts it reports. It never takes clicks: orbit, select and Cancel
// (in the header pill) all keep working underneath.

extension EditorModel {
    /// Parts landed / total while an opened document's first solve streams
    /// in; nil once it is done (or for a re-solve of an edit).
    var openingParts: (done: Int, total: Int)? {
        guard loading?.isEvaluating == true else { return nil }
        let total = solveProgress.total > 0 ? solveProgress.total : (documentGraph?.visibleRoots.count ?? 0)
        return (min(solveProgress.done, total), total)
    }

    /// The part has not landed yet in the first solve. Roots land in
    /// document order, so `done` of them are in (a refused root lands too,
    /// empty — it never spins forever).
    func isPartPending(_ part: Int) -> Bool {
        guard let p = openingParts else { return false }
        return part >= p.done
    }

    /// The part the first solve is working on now.
    func isPartSolving(_ part: Int) -> Bool {
        guard let p = openingParts, p.total > 0 else { return false }
        return part == p.done
    }

    /// The name of the `part`-th visible root, as the model tree shows it.
    func visiblePartName(_ part: Int) -> String? {
        featureNodes.first { $0.partIndex == part }?.name
    }
}

struct OpeningOverlay: View {
    @Bindable var model: EditorModel
    /// Bottom inset for the folded chip (clears the dock).
    var chipClearance: CGFloat

    var body: some View {
        if let load = model.loading, load.isRunning {
            let compact = load.landedGeometry
            content(load, compact: compact)
                .frame(maxWidth: .infinity, maxHeight: .infinity,
                       alignment: compact ? .bottom : .center)
                .padding(.bottom, compact ? chipClearance : 0)
                .allowsHitTesting(false)
                .transition(.opacity)
                .animation(Motion.panel, value: compact)
        }
    }

    @ViewBuilder private func content(_ load: LoadProgress, compact: Bool) -> some View {
        let parts = model.openingParts
        if compact {
            HStack(spacing: Theme.Space.s) {
                ProgressView().controlSize(.small)
                stageText(load, parts)
                elapsed(load)
            }
            .font(.callout)
            .padding(.horizontal, 14).padding(.vertical, 8)
            .pillSurface()
            .accessibilityElement(children: .combine)
        } else {
            VStack(alignment: .leading, spacing: Theme.Space.s) {
                HStack(spacing: Theme.Space.s) {
                    Image(systemName: load.kind.symbol).foregroundStyle(.secondary)
                    Text(load.fileName).font(.headline).lineLimit(1).truncationMode(.middle)
                    Spacer(minLength: Theme.Space.s)
                    elapsed(load)
                }
                stageText(load, parts).font(.callout)
                if let parts, parts.total > 1 {
                    PartStrip(done: parts.done, total: parts.total)
                } else {
                    PartStrip(done: 0, total: 1)       // one shimmering bar
                }
            }
            .padding(Theme.Space.l)
            .frame(width: 300)
            .panelSurface()
            .accessibilityElement(children: .combine)
        }
    }

    /// "Evaluating lid-flange · 2 of 9", "Finishing", "Building faces".
    private func stageText(_ load: LoadProgress, _ parts: (done: Int, total: Int)?) -> some View {
        let line: String
        if let parts, parts.total > 1 {
            if parts.done >= parts.total {
                line = "Finishing · \(parts.total) parts"
            } else {
                let name = model.visiblePartName(parts.done).map { " \($0)" } ?? ""
                line = "\(load.kind.imports ? "Meshing" : "Evaluating")\(name) · \(parts.done + 1) of \(parts.total)"
            }
        } else {
            line = load.stageLabel
        }
        return Text(line).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
            .contentTransition(.opacity)
    }

    /// "4 s" — ticks every second from the click.
    private func elapsed(_ load: LoadProgress) -> some View {
        TimelineView(.periodic(from: load.started, by: 1)) { ctx in
            let s = Int(ctx.date.timeIntervalSince(load.started))
            Text(s < 60 ? "\(s) s" : String(format: "%d:%02d", s / 60, s % 60))
                .font(.caption.monospacedDigit()).foregroundStyle(.tertiary)
        }
    }
}

/// One segment per part: landed parts filled, the solving one shimmering,
/// the rest waiting. Past a few dozen parts the segments merge into a bar.
private struct PartStrip: View {
    let done: Int
    let total: Int
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let segmented = total > 1 && total <= 48
        HStack(spacing: segmented ? 3 : 0) {
            if segmented {
                ForEach(0..<total, id: \.self) { i in
                    segment(i < done ? .landed : i == done ? .solving : .waiting)
                }
            } else {
                GeometryReader { geo in
                    let f = total > 0 ? CGFloat(done) / CGFloat(total) : 0
                    ZStack(alignment: .leading) {
                        segment(.solving)
                        Capsule().fill(Color.accentColor).frame(width: geo.size.width * f)
                    }
                }
            }
        }
        .frame(height: 4)
        .animation(Motion.snappy, value: done)
    }

    private enum State { case landed, solving, waiting }

    @ViewBuilder private func segment(_ s: State) -> some View {
        switch s {
        case .landed:
            Capsule().fill(Color.accentColor)
        case .waiting:
            Capsule().fill(.quaternary)
        case .solving:
            if reduceMotion {
                Capsule().fill(Color.accentColor.opacity(0.45))
            } else {
                TimelineView(.animation(minimumInterval: 1.0 / 30)) { ctx in
                    let t = ctx.date.timeIntervalSinceReferenceDate
                    let phase = CGFloat(t.truncatingRemainder(dividingBy: 1.4) / 1.4)
                    Capsule().fill(.quaternary).overlay {
                        GeometryReader { geo in
                            LinearGradient(colors: [.clear, Color.accentColor.opacity(0.85), .clear],
                                           startPoint: .leading, endPoint: .trailing)
                                .frame(width: max(geo.size.width * 0.6, 12))
                                .offset(x: (geo.size.width * 1.6) * phase - geo.size.width * 0.6)
                        }
                        .clipShape(Capsule())
                    }
                }
            }
        }
    }
}
