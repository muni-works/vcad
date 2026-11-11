import SwiftUI

// Opening a NEW document — a double-clicked STEP file, a .vcad or loon file
// from Finder or Recents — goes through up to two slow stages: the kernel's
// STEP read (faces built N of M) and the first solve (parts landed N of M).
// Until something lands the stage shows only the grid and a glass card names
// the file and says what is happening; once the first part is visible the
// card folds to a compact pill under the title; on completion it fades out.
// Re-solves of an open document (parameter edits) never get the card: they
// keep the toolbar's lightweight "Solving" line.
//
// `LoadProgress` is the pure model (unit-tested in LoadProgressTests); the
// views below draw it with SwiftUI-only animation, so nothing here keeps the
// RealityKit renderer awake (see RenderGate).

/// The state of one document open, from first click to first full solve.
struct LoadProgress: Equatable {
    enum Kind: Equatable {
        case step, urdf, vcad, loon

        init(url: URL) {
            switch url.pathExtension.lowercased() {
            case "step", "stp": self = .step
            case "urdf": self = .urdf
            case "loon": self = .loon
            default: self = url.hasDirectoryPath ? .loon : .vcad
            }
        }

        /// SF Symbol for the file card.
        var symbol: String {
            switch self {
            case .step: return "cube.transparent"
            case .urdf: return "figure.walk.motion"
            case .vcad: return "cube"
            case .loon: return "chevron.left.forwardslash.chevron.right"
            }
        }

        /// The short format badge.
        var badge: String {
            switch self {
            case .step: return "STEP"
            case .urdf: return "URDF"
            case .vcad: return "VCAD"
            case .loon: return "LOON"
            }
        }

        /// Whether a kernel import runs before the first solve.
        var imports: Bool { self == .step }
    }

    enum Phase: Equatable {
        /// STEP: reading the file's bytes.
        case reading
        /// STEP: tokenising entities — one pass, no count.
        case parsing
        /// STEP: building B-rep faces.
        case faces(done: Int, total: Int)
        /// First solve: parts landed of total (0 of 0 before the kernel says).
        case evaluating(done: Int, total: Int)
        /// Stopped; the card stays up with the message.
        case failed(message: String, details: String?)
        /// Opened, but with something worth saying (STEP faces skipped).
        case warning(message: String, details: String?)
    }

    enum Presentation: Equatable { case card, pill }

    let url: URL
    let kind: Kind
    let started: Date
    var phase: Phase
    /// A part is on screen (the card folds to the pill).
    var landedGeometry = false

    init(url: URL, kind: Kind? = nil, started: Date = Date(), phase: Phase? = nil) {
        self.url = url
        let k = kind ?? Kind(url: url)
        self.kind = k
        self.started = started
        self.phase = phase ?? (k.imports ? .reading : .evaluating(done: 0, total: 0))
    }

    var fileName: String { url.lastPathComponent }

    /// Still before the document switch: the previous source is the model's
    /// `source`, and the stage must not show its geometry.
    var isImporting: Bool {
        switch phase {
        case .reading, .parsing, .faces: return true
        default: return false
        }
    }
    var isEvaluating: Bool { if case .evaluating = phase { return true }; return false }
    var isFailed: Bool { if case .failed = phase { return true }; return false }
    /// Work is still running (Cancel applies).
    var isRunning: Bool { isImporting || isEvaluating }

    /// Import's share of the whole bar when the open has two stages.
    static let importShare = 0.55

    /// Overall fraction 0…1, or nil when there is nothing to measure (the
    /// view draws an indeterminate shimmer then).
    var fraction: Double? {
        func ratio(_ d: Int, _ t: Int) -> Double { t > 0 ? min(1, max(0, Double(d) / Double(t))) : 0 }
        switch phase {
        case .reading, .parsing:
            return nil
        case let .faces(d, t):
            guard t > 0 else { return nil }
            return ratio(d, t) * Self.importShare
        case let .evaluating(d, t):
            // A single-part solve has no steps to count.
            guard t > 1 else { return nil }
            let r = ratio(d, t)
            return kind.imports ? Self.importShare + r * (1 - Self.importShare) : r
        case .failed, .warning:
            return nil
        }
    }

    /// The current stage, in a few words.
    var stageLabel: String {
        switch phase {
        case .reading: return "Reading file"
        case .parsing: return "Parsing entities"
        case let .faces(d, t): return t > 0 ? "Building faces \(d) of \(t)" : "Building faces"
        case let .evaluating(d, t):
            let verb = kind.imports ? "Meshing" : "Evaluating"
            return t > 1 ? "\(verb) parts \(min(d, t)) of \(t)" : (kind.imports ? "Meshing" : "Evaluating")
        case let .failed(message, _): return message
        case let .warning(message, _): return message
        }
    }

    var details: String? {
        switch phase {
        case let .failed(_, d), let .warning(_, d): return d
        default: return nil
        }
    }

    var presentation: Presentation {
        switch phase {
        case .failed, .warning: return .card
        default: return landedGeometry ? .pill : .card
        }
    }

    /// "12 s" / "1:04", only once the open has taken a noticeable while.
    func elapsedLabel(now: Date) -> String? {
        let s = now.timeIntervalSince(started)
        guard s >= 2, isRunning else { return nil }
        let whole = Int(s)
        return whole < 60 ? "\(whole) s" : String(format: "%d:%02d", whole / 60, whole % 60)
    }

    /// The STEP job's stage code (`VCAD_STEP_STAGE_*`) as a phase.
    static func importPhase(stage: UInt32, done: Int, total: Int) -> Phase {
        switch stage {
        case 0: return .reading
        case 1: return .parsing
        default: return .faces(done: done, total: total)
        }
    }

    /// A kernel error string as a short message + the full text for details.
    static func failure(_ raw: String?, fallback: String) -> Phase {
        let text = (raw?.isEmpty == false ? raw! : fallback)
        let first = text.split(separator: "\n").first.map(String.init) ?? text
        let short = first.count > 90 ? String(first.prefix(87)) + "…" : first
        return .failed(message: short, details: text == short ? nil : text)
    }

    /// The import report's summary (nil when clean) as a warning phase.
    static func importWarning(skippedFaces: Int, report: String?) -> Phase? {
        guard skippedFaces > 0 else { return nil }
        let noun = skippedFaces == 1 ? "face" : "faces"
        return .warning(message: "Opened with \(skippedFaces) \(noun) skipped — the model has holes",
                        details: report)
    }
}

// MARK: - Views

/// The card, or the pill once geometry is visible. Hosted over the viewport.
struct LoadProgressOverlay: View {
    @Bindable var model: EditorModel
    /// Floating chrome starts below the toolbar row.
    let top: CGFloat

    @State private var shown = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        ZStack {
            if let load = model.loading, shown || !load.isRunning {
                switch load.presentation {
                case .card:
                    LoadCard(load: load,
                             cancel: { model.cancelLoading() },
                             retry: { model.retryLoading() },
                             close: { model.dismissLoading() })
                        .modifier(ChromeRegion(key: "loadCard"))
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .transition(.scale(scale: 0.96).combined(with: .opacity))
                case .pill:
                    VStack {
                        LoadPill(load: load, cancel: { model.cancelLoading() })
                            .modifier(ChromeRegion(key: "loadPill"))
                            .padding(.top, top + Theme.Space.s)
                        Spacer()
                    }
                    .frame(maxWidth: .infinity)
                    .transition(.move(edge: .top).combined(with: .opacity))
                }
            }
        }
        .animation(glassSpring(reduceMotion), value: model.loading?.presentation)
        .animation(glassSpring(reduceMotion), value: model.loading == nil)
        // A cache-hit open finishes before anyone could read the card: only
        // show it once the open has run for a beat.
        .task(id: model.loading?.started) {
            shown = false
            guard model.loading != nil else { return }
            try? await Task.sleep(for: .milliseconds(250))
            withAnimation(glassSpring(reduceMotion)) { shown = true }
        }
    }
}

private struct LoadCard: View {
    let load: LoadProgress
    let cancel: () -> Void
    let retry: () -> Void
    let close: () -> Void
    @State private var showsDetails = false

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            HStack(spacing: Theme.Space.m) {
                FileGlyph(kind: load.kind, tint: tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text(load.fileName)
                        .font(.headline)
                        .lineLimit(1).truncationMode(.middle)
                    Text(headline)
                        .font(.subheadline).foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
            }
            if load.isRunning {
                LoadBar(fraction: load.fraction)
                HStack {
                    Text(load.stageLabel)
                        .contentTransition(.numericText())
                        .monospacedDigit()
                    Spacer()
                    TimelineView(.periodic(from: load.started, by: 1)) { ctx in
                        Text(load.elapsedLabel(now: ctx.date) ?? "")
                            .monospacedDigit()
                    }
                }
                .font(.caption).foregroundStyle(.secondary)
                .animation(.smooth(duration: 0.2), value: load.stageLabel)
            } else {
                HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s) {
                    Image(systemName: load.isFailed ? "exclamationmark.octagon.fill"
                                                    : "exclamationmark.triangle.fill")
                        .foregroundStyle(tint)
                    Text(load.stageLabel)
                        .font(.callout)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
                if showsDetails, let d = load.details {
                    ScrollView {
                        Text(d)
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .frame(maxHeight: 140)
                    .padding(Theme.Space.s)
                    .background(.quaternary.opacity(0.5),
                                in: RoundedRectangle(cornerRadius: Theme.Radius.control, style: .continuous))
                }
            }
            HStack(spacing: Theme.Space.s) {
                if !load.isRunning, load.details != nil {
                    Button(showsDetails ? "Hide Details" : "Show Details") {
                        withAnimation(.smooth(duration: 0.25)) { showsDetails.toggle() }
                    }
                    .buttonStyle(.link).font(.callout)
                }
                Spacer()
                if load.isRunning {
                    Button("Cancel", action: cancel)
                        .controlSize(.large)
                } else if load.isFailed {
                    Button("Close", action: close).controlSize(.large)
                    Button("Retry", action: retry)
                        .keyboardShortcut(.defaultAction)
                        .controlSize(.large)
                } else {
                    Button("OK", action: close)
                        .keyboardShortcut(.defaultAction)
                        .controlSize(.large)
                }
            }
        }
        .padding(Theme.Space.l + 4)
        .frame(width: 360)
        .glassPanel()
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Opening \(load.fileName)")
    }

    private var headline: String {
        switch load.phase {
        case .failed: return "Couldn’t open"
        case .warning: return "Opened with warnings"
        default: return load.kind.imports && load.isImporting ? "Importing \(load.kind.badge)" : "Opening"
        }
    }

    private var tint: Color {
        switch load.phase {
        case .failed: return .red
        case .warning: return .orange
        default: return .accentColor
        }
    }
}

/// Compact: under the title once the first part is visible.
private struct LoadPill: View {
    let load: LoadProgress
    let cancel: () -> Void

    var body: some View {
        HStack(spacing: Theme.Space.s) {
            ProgressRing(fraction: load.fraction)
            Text(load.stageLabel)
                .monospacedDigit()
                .contentTransition(.numericText())
                .animation(.smooth(duration: 0.2), value: load.stageLabel)
            Button(action: cancel) {
                Image(systemName: "xmark").font(.caption.weight(.semibold))
            }
            .buttonStyle(.plain).foregroundStyle(.secondary)
            .help("Stop opening \(load.fileName)")
        }
        .font(.callout)
        .padding(.horizontal, Theme.Space.m).padding(.vertical, 7)
        .glassCapsule()
    }
}

private struct FileGlyph: View {
    let kind: LoadProgress.Kind
    let tint: Color

    var body: some View {
        ZStack(alignment: .bottom) {
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .fill(tint.opacity(0.14))
                .frame(width: 44, height: 50)
            Image(systemName: kind.symbol)
                .font(.system(size: 20, weight: .medium))
                .foregroundStyle(tint)
                .frame(width: 44, height: 50)
                .offset(y: -5)
            Text(kind.badge)
                .font(.system(size: 8, weight: .bold, design: .rounded))
                .kerning(0.4)
                .foregroundStyle(tint)
                .padding(.bottom, 5)
        }
        .accessibilityHidden(true)
    }
}

/// Determinate when it can be; a soft shimmer only when nothing is measurable.
struct LoadBar: View {
    let fraction: Double?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(.quaternary)
                if let f = fraction {
                    Capsule().fill(Color.accentColor)
                        .frame(width: max(6, geo.size.width * f))
                        .animation(.smooth(duration: 0.35), value: f)
                } else {
                    Shimmer(width: geo.size.width, reduceMotion: reduceMotion)
                }
            }
            .clipShape(Capsule())
        }
        .frame(height: 5)
        .accessibilityElement()
        .accessibilityLabel("Progress")
        .accessibilityValue(fraction.map { "\(Int($0 * 100)) percent" } ?? "In progress")
    }
}

private struct Shimmer: View {
    let width: CGFloat
    let reduceMotion: Bool
    @State private var phase: CGFloat = -0.4

    var body: some View {
        let band = max(40, width * 0.35)
        Capsule()
            .fill(LinearGradient(colors: [.accentColor.opacity(0), .accentColor.opacity(0.85), .accentColor.opacity(0)],
                                 startPoint: .leading, endPoint: .trailing))
            .frame(width: band)
            .offset(x: reduceMotion ? (width - band) / 2 : phase * width)
            .onAppear {
                guard !reduceMotion else { return }
                withAnimation(.easeInOut(duration: 1.3).repeatForever(autoreverses: false)) { phase = 1.0 }
            }
    }
}

private struct ProgressRing: View {
    let fraction: Double?

    var body: some View {
        if let f = fraction {
            ZStack {
                Circle().stroke(.quaternary, lineWidth: 2.5)
                Circle().trim(from: 0, to: f)
                    .stroke(Color.accentColor, style: StrokeStyle(lineWidth: 2.5, lineCap: .round))
                    .rotationEffect(.degrees(-90))
                    .animation(.smooth(duration: 0.35), value: f)
            }
            .frame(width: 14, height: 14)
        } else {
            ProgressView().controlSize(.mini)
        }
    }
}
