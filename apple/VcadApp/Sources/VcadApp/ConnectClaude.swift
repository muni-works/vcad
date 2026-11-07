#if canImport(AppKit)
import AppKit
#endif
import Foundation

// "Connect Claude…": register the app's MCP server with Claude Code and
// Claude Desktop, then prove the path works end to end by speaking MCP
// through the very shim the configs point at.
//
// Both clients get the same stdio entry:
//
//   "vcad-app": { "command": "<abs node>", "args": ["<abs shim path>"] }
//
// - Claude Code: user scope, the top-level `mcpServers` of ~/.claude.json.
// - Claude Desktop: `mcpServers` of
//   ~/Library/Application Support/Claude/claude_desktop_config.json.
//
// Existing keys are merged, never dropped; each file is backed up once to
// `<file>.vcad-backup` before the first write. Claude Desktop launches MCP
// servers with a bare PATH, so `node` is written as an absolute path.

enum ConnectClaude {
    static let serverName = "vcad-app"

    struct Target: Equatable {
        let label: String
        let config: URL
    }

    static var home: URL { FileManager.default.homeDirectoryForCurrentUser }

    static func targets(home: URL = ConnectClaude.home) -> [Target] {
        [Target(label: "Claude Code", config: home.appendingPathComponent(".claude.json")),
         Target(label: "Claude Desktop",
                config: home.appendingPathComponent("Library/Application Support/Claude/claude_desktop_config.json"))]
    }

    /// The shim: bundled in Resources for a shipped app, else the repo copy
    /// next to this source file (swift run / tests).
    static func shimPath() -> String? {
        if let p = Bundle.main.path(forResource: "vcad-app-mcp", ofType: "mjs") { return p }
        let repo = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("scripts/vcad-app-mcp.mjs")
        return FileManager.default.fileExists(atPath: repo.path) ? repo.path : nil
    }

    /// Absolute `node`: common install locations, then the login shell's PATH.
    static func nodePath() -> String? {
        let fm = FileManager.default
        let candidates = ["/opt/homebrew/bin/node", "/usr/local/bin/node", "\(home.path)/.volta/bin/node"]
        if let hit = candidates.first(where: { fm.isExecutableFile(atPath: $0) }) { return hit }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/zsh")
        p.arguments = ["-lc", "command -v node"]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return nil }
        p.waitUntilExit()
        let s = String(decoding: out.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return fm.isExecutableFile(atPath: s) ? s : nil
    }

    static func entry(node: String, shim: String) -> [String: Any] {
        ["command": node, "args": [shim]]
    }

    /// Merge our server into `mcpServers` of the JSON config at `url`,
    /// creating the file if needed. Returns true when the file changed.
    @discardableResult
    static func install(entry: [String: Any], into url: URL) throws -> Bool {
        let fm = FileManager.default
        var root: [String: Any] = [:]
        if let data = try? Data(contentsOf: url), !data.isEmpty {
            guard let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                throw CocoaError(.fileReadCorruptFile, userInfo: [NSFilePathErrorKey: url.path])
            }
            root = obj
            let backup = url.appendingPathExtension("vcad-backup")
            if !fm.fileExists(atPath: backup.path) { try? data.write(to: backup) }
        }
        var servers = root["mcpServers"] as? [String: Any] ?? [:]
        if let old = servers[serverName] as? [String: Any], NSDictionary(dictionary: old).isEqual(to: entry) {
            return false
        }
        servers[serverName] = entry
        root["mcpServers"] = servers
        try fm.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let out = try JSONSerialization.data(withJSONObject: root, options: [.prettyPrinted, .sortedKeys])
        try out.write(to: url, options: .atomic)
        return true
    }

    /// Speak MCP `initialize` + `tools/list` through the shim, the way a
    /// client will. Returns the server name and tool count. Runs the child off
    /// the main thread: the bridge answers on the main queue.
    static func handshake(node: String, shim: String, timeout: TimeInterval = 8) async throws -> (server: String, tools: Int) {
        try await Task.detached {
            let p = Process()
            p.executableURL = URL(fileURLWithPath: node)
            p.arguments = [shim]
            let inPipe = Pipe(), outPipe = Pipe()
            p.standardInput = inPipe
            p.standardOutput = outPipe
            p.standardError = FileHandle.nullDevice
            try p.run()
            defer { if p.isRunning { p.terminate() } }
            let msgs = [
                #"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"vcad-connect","version":"1"}}}"#,
                #"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                #"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            ]
            inPipe.fileHandleForWriting.write(Data((msgs.joined(separator: "\n") + "\n").utf8))
            var buf = Data()
            var replies: [Int: [String: Any]] = [:]
            let deadline = Date().addingTimeInterval(timeout)
            let fd = outPipe.fileHandleForReading
            while replies.count < 2, Date() < deadline {
                let chunk = fd.availableData
                if chunk.isEmpty { break }
                buf.append(chunk)
                while let nl = buf.firstIndex(of: 0x0A) {
                    let line = buf[buf.startIndex..<nl]
                    buf.removeSubrange(buf.startIndex...nl)
                    if let o = (try? JSONSerialization.jsonObject(with: line)) as? [String: Any],
                       let id = o["id"] as? Int { replies[id] = o }
                }
            }
            try? inPipe.fileHandleForWriting.close()
            guard let initR = replies[1] else { throw HandshakeError(message: "no reply from the shim") }
            if let e = initR["error"] as? [String: Any] {
                throw HandshakeError(message: e["message"] as? String ?? "initialize failed")
            }
            let name = ((initR["result"] as? [String: Any])?["serverInfo"] as? [String: Any])?["name"] as? String ?? "?"
            let tools = ((replies[2]?["result"] as? [String: Any])?["tools"] as? [Any])?.count ?? 0
            return (name, tools)
        }.value
    }

    struct HandshakeError: Error, CustomStringConvertible { let message: String; var description: String { message } }

    // MARK: menu action

    /// Write both configs, verify the handshake, and say what happened.
    @MainActor
    static func run() {
        guard let shim = shimPath() else { return alert("Couldn't find the MCP shim (vcad-app-mcp.mjs) in the app bundle.", ok: false) }
        guard let node = nodePath() else {
            return alert("Connecting Claude needs Node.js (the MCP shim is a small node script). Install it from nodejs.org or with `brew install node`, then try again.", ok: false)
        }
        var lines: [String] = []
        for t in targets() {
            do {
                let changed = try install(entry: entry(node: node, shim: shim), into: t.config)
                lines.append("✓ \(t.label): \(changed ? "added" : "already set up") (\(t.config.path.replacingOccurrences(of: home.path, with: "~")))")
            } catch {
                lines.append("✗ \(t.label): couldn't write \(t.config.lastPathComponent) — \(error.localizedDescription)")
            }
        }
        Task { @MainActor in
            do {
                let (server, tools) = try await handshake(node: node, shim: shim)
                lines.append("✓ Handshake: \(server) answered with \(tools) tools.")
                lines.append("\nRestart Claude Desktop, or start a new Claude Code session, and ask Claude to model something — it edits this window live.")
                alert(lines.joined(separator: "\n"), ok: true)
            } catch {
                lines.append("✗ Handshake failed: \(error)")
                alert(lines.joined(separator: "\n"), ok: false)
            }
        }
    }

    @MainActor
    private static func alert(_ text: String, ok: Bool) {
        #if canImport(AppKit)
        let a = NSAlert()
        a.messageText = ok ? "Claude is connected" : "Couldn't connect Claude"
        a.informativeText = text
        a.alertStyle = ok ? .informational : .warning
        a.runModal()
        #endif
    }
}
