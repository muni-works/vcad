import SwiftUI

// Opening a NEW document — a double-clicked STEP file, a .vcad or loon file
// from Finder or Recents — goes through up to two slow stages: the kernel's
// STEP read (faces built N of M) and the first solve (parts landed N of M).
// While it runs it is one task ("load") in the window's ProgressCenter, drawn
// by the header title pill like every other long operation; a failure or
// warning stays readable in that same pill (GlassToolbar).
//
// `LoadProgress` is the pure model (unit-tested in LoadProgressTests).

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

// MARK: - As a progress task (ProgressCenter)

extension LoadProgress {
    /// The pill's task title: "Importing STEP", then "Opening".
    var taskTitle: String {
        isImporting ? "Importing \(kind.badge)" : "Opening"
    }

    /// The step within it, without counts (the pill adds those).
    var taskStage: String? {
        switch phase {
        case .reading: return "Reading file"
        case .parsing: return "Parsing entities"
        case .faces: return "Building faces"
        case let .evaluating(d, t):
            guard t > 1 else { return kind.imports ? "Meshing" : "Evaluating" }
            // Every part is in; the kernel's closing pass has no count.
            if d >= t { return "Finishing" }
            return kind.imports ? "Meshing parts" : "Evaluating parts"
        case .failed, .warning: return nil
        }
    }

    /// Counted progress of the current stage (none while finishing).
    var taskCounts: (done: Int, total: Int) {
        switch phase {
        case let .evaluating(d, t) where d >= t: return (0, 0)
        case let .faces(d, t), let .evaluating(d, t): return (min(d, t), t)
        default: return (0, 0)
        }
    }

    /// The bar: overall fraction, indeterminate while finishing (a full bar
    /// that doesn't end reads as stuck).
    var taskFraction: Double? {
        if case let .evaluating(d, t) = phase, t > 1, d >= t { return nil }
        return fraction
    }
}
