import CryptoKit
import Foundation

// The Welcome window's pure logic, kept free of AppKit/SwiftUI so it can be
// unit tested: which recents to show, first launch vs returning, the "Show at
// launch" preference, live agent status, keyboard movement through the card
// grids, and the thumbnail cache key. The view is in Welcome.swift.

/// First launch shows the teaching examples up top; a returning user (anything
/// in Recent that still exists) sees Recent first.
enum WelcomeMode: Equatable, Sendable {
    case firstLaunch
    case returning

    init(recents: [WelcomeRecent]) {
        self = recents.isEmpty ? .firstLaunch : .returning
    }

    /// The examples section's label: a teaching invitation on first launch,
    /// a quieter heading once the user has their own work.
    var examplesTitle: String {
        switch self {
        case .firstLaunch: "Start from an example — each one teaches something"
        case .returning: "Examples"
        }
    }
}

/// One recent document as the Welcome window lists it.
struct WelcomeRecent: Identifiable, Equatable, Sendable {
    let url: URL
    let modified: Date?
    var id: String { url.path }
    var name: String { url.deletingPathExtension().lastPathComponent }
}

enum WelcomeRecents {
    /// The Recent grid holds one row of cards.
    static let limit = 4

    /// Stored paths (most recent first) → the cards to show: duplicates and
    /// files that no longer exist dropped, capped at `limit`.
    static func list(paths: [String], limit: Int = WelcomeRecents.limit,
                     fileManager fm: FileManager = .default) -> [WelcomeRecent] {
        var seen = Set<String>()
        var out: [WelcomeRecent] = []
        for path in paths where out.count < limit {
            let url = URL(fileURLWithPath: path).standardizedFileURL
            guard seen.insert(url.path).inserted, fm.fileExists(atPath: url.path) else { continue }
            let modified = (try? fm.attributesOfItem(atPath: url.path))?[.modificationDate] as? Date
            out.append(WelcomeRecent(url: url, modified: modified))
        }
        return out
    }

    /// The line under a recent's name: "9 parts · yesterday". Either half may
    /// be missing (parts unknown until the thumbnail renders).
    static func context(parts: Int?, modified: Date?, now: Date = Date(),
                        calendar: Calendar = .current) -> String {
        var bits: [String] = []
        if let parts, parts > 0 { bits.append(counted(parts, "part")) }
        if let modified { bits.append(relativeDay(modified, now: now, calendar: calendar)) }
        return bits.joined(separator: " · ")
    }

    /// "today", "yesterday", "3 days ago", or a short date beyond a week.
    static func relativeDay(_ date: Date, now: Date, calendar: Calendar = .current) -> String {
        let days = calendar.dateComponents([.day], from: calendar.startOfDay(for: date),
                                           to: calendar.startOfDay(for: now)).day ?? 0
        switch days {
        case ..<1: return "today"
        case 1: return "yesterday"
        case 2..<7: return "\(days) days ago"
        default:
            let f = DateFormatter()
            f.calendar = calendar
            f.timeZone = calendar.timeZone
            f.setLocalizedDateFormatFromTemplate("MMMd")
            return f.string(from: date)
        }
    }
}

/// "Show at launch": on by default, remembered once the user flips it.
enum WelcomePrefs {
    static let showAtLaunchKey = "vcad.welcome.showAtLaunch"

    static func showAtLaunch(_ defaults: UserDefaults = .standard) -> Bool {
        defaults.object(forKey: showAtLaunchKey) == nil ? true : defaults.bool(forKey: showAtLaunchKey)
    }

    static func setShowAtLaunch(_ on: Bool, _ defaults: UserDefaults = .standard) {
        defaults.set(on, forKey: showAtLaunchKey)
    }

    /// Whether a fresh instance should open the Welcome window: the preference
    /// is on, the instance is an empty scratch (not opened for a document),
    /// and no headless dev hook is driving the app.
    static func shouldShowAtLaunch(isScratch: Bool, defaults: UserDefaults = .standard,
                                   environment: [String: String] = ProcessInfo.processInfo.environment) -> Bool {
        guard isScratch, showAtLaunch(defaults) else { return false }
        if environment["VCAD_NO_WELCOME"] == "1" { return false }
        if environment["VCAD_WELCOME"] == "1" { return true }
        // Smoke/dev hooks drive the app without a person in front of it.
        return !environment.keys.contains {
            $0.hasPrefix("VCAD_") && !["VCAD_LOON_PATH", "VCAD_SAMPLES"].contains($0)
        }
    }
}

/// The "Bring your agent" card's state.
enum WelcomeAgentState: Equatable, Sendable {
    /// No agent has vcad configured. `detected` lists installed agents that
    /// could be connected (the Connect button's offer).
    case notConnected(detected: [String])
    /// An agent has vcad in its config but hasn't called this instance lately.
    case connected(String)
    /// An agent called this instance within `activeWindow`.
    case active(String)

    static let activeWindow: TimeInterval = 120

    static func from(statuses: [(name: String, status: AgentStatus)], lastCall: Date?,
                     now: Date = Date()) -> WelcomeAgentState {
        let connected = statuses.filter {
            if case .foreign = $0.status { return true }
            return $0.status == .connected
        }.map(\.name)
        let name = connected.first ?? "Your agent"
        if let lastCall, now.timeIntervalSince(lastCall) < activeWindow { return .active(name) }
        if let first = connected.first { return .connected(first) }
        return .notConnected(detected: statuses.filter { $0.status == .available }.map(\.name))
    }

    var title: String {
        switch self {
        case .notConnected: "Not connected"
        case .connected(let n): "\(n) connected"
        case .active(let n): "\(n) is working here"
        }
    }

    var isConnected: Bool {
        if case .notConnected = self { return false }
        return true
    }
}

/// Arrow-key movement through a grid of `count` cards laid out `columns` wide.
/// Returns nil when the move leaves the grid (focus passes to the neighbour).
enum WelcomeGridNav {
    enum Move: Sendable { case left, right, up, down }

    static func move(_ index: Int, _ move: Move, columns: Int, count: Int) -> Int? {
        guard count > 0, columns > 0, (0..<count).contains(index) else { return nil }
        let next: Int
        switch move {
        case .left: guard index % columns > 0 else { return nil }; next = index - 1
        case .right: guard index % columns < columns - 1 else { return nil }; next = index + 1
        case .up: next = index - columns
        case .down: next = index + columns
        }
        return (0..<count).contains(next) ? next : nil
    }
}

/// Recent-document thumbnails live in ~/Library/Caches/vcad/thumbnails, keyed
/// by path + modification time + size, so an edited file re-renders and an
/// unchanged one never does.
enum WelcomeThumbnailKey {
    static func key(path: String, modified: Date?, size: Int?) -> String {
        let stamp = "\(path)|\(modified.map { Int($0.timeIntervalSince1970) } ?? 0)|\(size ?? 0)"
        return SHA256.hash(data: Data(stamp.utf8)).prefix(12).map { String(format: "%02x", $0) }.joined()
    }

    static func key(for url: URL, fileManager fm: FileManager = .default) -> String {
        let attrs = try? fm.attributesOfItem(atPath: url.path)
        return key(path: url.standardizedFileURL.path, modified: attrs?[.modificationDate] as? Date,
                   size: (attrs?[.size] as? NSNumber)?.intValue)
    }

    static var directory: URL {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("vcad/thumbnails", isDirectory: true)
    }
}

enum WelcomeExamples {
    /// The "Start from an example" cards: the samples that teach something,
    /// in manifest order, one row of four.
    static func pick(_ samples: [SampleProject], limit: Int = 4) -> [SampleProject] {
        Array(samples.filter { $0.lesson != nil }.prefix(limit))
    }
}
