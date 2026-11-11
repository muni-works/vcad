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

    enum CodingKeys: String, CodingKey {
        case pid, port, url, started
        case instanceId = "instance_id"
        case documentName = "document_name"
        case documentPath = "document_path"
    }
}

enum AgentDiscovery {
    static func root(home: URL = FileManager.default.homeDirectoryForCurrentUser) -> URL {
        home.appendingPathComponent(".vcad")
    }
    static func instancesDir(home: URL = FileManager.default.homeDirectoryForCurrentUser) -> URL {
        root(home: home).appendingPathComponent("instances")
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
