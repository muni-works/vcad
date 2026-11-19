import Darwin
import Foundation

// Agent-bridge discovery, one file per running instance.
//
//   ~/.vcad/instances/<pid>.json   one per live vcad.app (removed on exit;
//                                  dead pids are pruned by every writer)
//   ~/.vcad/app.json               "most recent" pointer, kept for old clients
//
// The old single slot meant two windows (or a relaunch) silently redirected
// every other agent's bridge calls to the wrong app. Clients now pick a target
// explicitly — VCAD_APP_PID / VCAD_APP_URL, else the instance whose document
// matches, else the newest with a warning — see
// apple/VcadApp/scripts/vcad-app-discovery.mjs and packages/mcp/src/app-discovery.ts.
//
// Throwaway runs (screenshot/perf/bake harnesses, dev bundles a script kills a
// minute later) must not win that "newest" pick and strand a real agent
// session: they don't advertise at all (`advertises(env:)`), and every record
// carries `bundle_id` so clients skip `io.vcad.dev.*` builds unless named.

/// One running instance, as written to `instances/<pid>.json`.
struct AgentInstanceRecord: Codable, Equatable {
    var pid: Int
    var port: Int
    var url: String
    /// Random per launch: a changed id at the same port means the app relaunched.
    var instanceId: String
    /// ISO-8601 launch time.
    var started: String
    var documentName: String?
    var documentPath: String?
    /// CFBundleIdentifier: `io.vcad.m0` for releases, `io.vcad.dev.<checkout>`
    /// for bundle.sh dev builds — which clients skip unless named explicitly.
    var bundleId: String? = nil

    enum CodingKeys: String, CodingKey {
        case pid, port, url, started
        case instanceId = "instance_id"
        case documentName = "document_name"
        case documentPath = "document_path"
        case bundleId = "bundle_id"
    }
}

enum AgentDiscovery {
    static func root(home: URL = FileManager.default.homeDirectoryForCurrentUser) -> URL {
        home.appendingPathComponent(".vcad")
    }
    static func instancesDir(home: URL = FileManager.default.homeDirectoryForCurrentUser) -> URL {
        root(home: home).appendingPathComponent("instances")
    }

    /// Scripted runs that open, snap or measure, then get killed. They never
    /// host a real agent session, so they stay out of discovery.
    static let throwawayRunKeys = [
        "VCAD_PERF_EXIT", "VCAD_BAKE_MESH_BUNDLE", "VCAD_SNAPSHOT_DIR",
        "VCAD_ASSEMBLY_SNAPSHOT_DIR", "VCAD_FIELDS_SNAPSHOT_DIR", "VCAD_FIELDS_SNAPSHOT_EXIT",
        "VCAD_UI_AUDIT_DIR",
    ]

    /// An XCTest host (tests that show the desktop start the bridge too).
    static let isUnderTest = NSClassFromString("XCTestCase") != nil

    /// Whether this process writes `instances/<pid>.json` + `app.json`.
    /// `VCAD_AGENT_DISCOVERY=0` serves the bridge without advertising it (a
    /// harness that drives the app over MCP pins `VCAD_AGENT_PORT` and talks
    /// to that port directly); throwaway runs and test hosts never advertise.
    static func advertises(env: [String: String] = ProcessInfo.processInfo.environment,
                           underTest: Bool = isUnderTest) -> Bool {
        if underTest || env["VCAD_AGENT_DISCOVERY"] == "0" { return false }
        return !throwawayRunKeys.contains { !(env[$0] ?? "").isEmpty }
    }

    static func isAlive(_ pid: Int) -> Bool {
        guard pid > 0 else { return false }
        return kill(pid_t(pid), 0) == 0 || errno == EPERM
    }

    private static func encode(_ r: AgentInstanceRecord) -> Data? {
        let enc = JSONEncoder()
        enc.outputFormatting = [.prettyPrinted, .sortedKeys]
        return try? enc.encode(r)
    }

    /// Writes this instance's file and repoints app.json at it; prunes dead entries first.
    static func write(_ r: AgentInstanceRecord, home: URL = FileManager.default.homeDirectoryForCurrentUser,
                      isAlive: (Int) -> Bool = isAlive) {
        let dir = instancesDir(home: home)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        prune(home: home, isAlive: isAlive)
        guard let data = encode(r) else { return }
        try? data.write(to: dir.appendingPathComponent("\(r.pid).json"), options: .atomic)
        try? data.write(to: root(home: home).appendingPathComponent("app.json"), options: .atomic)
    }

    /// Removes this instance's file. If app.json pointed at it, repoint at the
    /// newest surviving instance (or remove it when none is left).
    static func remove(pid: Int, home: URL = FileManager.default.homeDirectoryForCurrentUser,
                       isAlive: (Int) -> Bool = isAlive) {
        try? FileManager.default.removeItem(at: instancesDir(home: home).appendingPathComponent("\(pid).json"))
        let pointer = root(home: home).appendingPathComponent("app.json")
        if let data = try? Data(contentsOf: pointer),
           let cur = try? JSONDecoder().decode(PidOnly.self, from: data), cur.pid != pid {
            return
        }
        let survivors = list(home: home).filter { $0.pid != pid && isAlive($0.pid) }
        if let newest = survivors.max(by: { $0.started < $1.started }), let data = encode(newest) {
            try? data.write(to: pointer, options: .atomic)
        } else {
            try? FileManager.default.removeItem(at: pointer)
        }
    }

    /// Deletes files whose pid is dead (crashed or force-killed instances).
    @discardableResult
    static func prune(home: URL = FileManager.default.homeDirectoryForCurrentUser,
                      isAlive: (Int) -> Bool = isAlive) -> [Int] {
        var removed: [Int] = []
        for r in list(home: home) where !isAlive(r.pid) {
            try? FileManager.default.removeItem(at: instancesDir(home: home).appendingPathComponent("\(r.pid).json"))
            removed.append(r.pid)
        }
        return removed
    }

    /// Every readable record (live or not), newest first.
    static func list(home: URL = FileManager.default.homeDirectoryForCurrentUser) -> [AgentInstanceRecord] {
        let dir = instancesDir(home: home)
        let files = (try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)) ?? []
        return files.filter { $0.pathExtension == "json" }
            .compactMap { try? JSONDecoder().decode(AgentInstanceRecord.self, from: Data(contentsOf: $0)) }
            .sorted { $0.started > $1.started }
    }

    private struct PidOnly: Decodable { let pid: Int }
}
