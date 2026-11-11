import Foundation

// One-breath install (M3): register vcad as an MCP server with every agent the
// user has installed, behind ONE consent dialog, undoable in Settings ▸ Agents.
//
// Agents and the file each one reads its MCP servers from:
//
//   Claude Code     ~/.claude.json                     mcpServers.vcad   (+ skill in ~/.claude/skills/vcad)
//   Claude Desktop  ~/Library/Application Support/Claude/claude_desktop_config.json   mcpServers.vcad
//   Cursor          ~/.cursor/mcp.json                 mcpServers.vcad
//   Codex           ~/.codex/config.toml               [mcp_servers.vcad] (marker-fenced block)
//
// Rules this file keeps:
//   - Nothing is written without the user's consent (the dialog, or a button in
//     Settings). Detection only reads.
//   - Existing content is merged, never dropped; each file is backed up once to
//     `<file>.vcad-backup` before vcad's first write.
//   - A `vcad` entry vcad.app didn't write (a contributor's serve.mjs, say) is
//     the user's: it is reported as such and never overwritten or removed.
//   - Undo removes exactly what vcad.app wrote, and only while it still matches.

enum AgentKind: String, CaseIterable, Identifiable, Sendable {
    case claudeCode, claudeDesktop, codex, cursor
    var id: String { rawValue }

    var label: String {
        switch self {
        case .claudeCode: "Claude Code"
        case .claudeDesktop: "Claude Desktop"
        case .codex: "Codex"
        case .cursor: "Cursor"
        }
    }

    /// The config file this agent reads MCP servers from.
    func config(home: URL) -> URL {
        switch self {
        case .claudeCode: home.appendingPathComponent(".claude.json")
        case .claudeDesktop: home.appendingPathComponent("Library/Application Support/Claude/claude_desktop_config.json")
        case .codex: home.appendingPathComponent(".codex/config.toml")
        case .cursor: home.appendingPathComponent(".cursor/mcp.json")
        }
    }

    /// Evidence the agent is installed: its config/state dir, or its .app.
    func evidence(home: URL) -> [String] {
        switch self {
        case .claudeCode: [home.appendingPathComponent(".claude.json").path, home.appendingPathComponent(".claude").path]
        case .claudeDesktop: ["/Applications/Claude.app", home.appendingPathComponent("Applications/Claude.app").path,
                              home.appendingPathComponent("Library/Application Support/Claude").path]
        case .codex: [home.appendingPathComponent(".codex").path, "/Applications/Codex.app"]
        case .cursor: [home.appendingPathComponent(".cursor").path, "/Applications/Cursor.app",
                       home.appendingPathComponent("Applications/Cursor.app").path]
        }
    }

    func isDetected(home: URL, fm: FileManager = .default) -> Bool {
        evidence(home: home).contains { fm.fileExists(atPath: $0) }
    }
}

/// The stdio command every agent is pointed at.
struct ServerEntry: Equatable, Sendable {
    var command: String
    var args: [String]
    var env: [String: String]

    var json: [String: Any] {
        var o: [String: Any] = ["command": command, "args": args]
        if !env.isEmpty { o["env"] = env }
        return o
    }
}

enum AgentStatus: Equatable, Sendable {
    case notDetected
    case available            // detected, no vcad entry
    case connected            // the entry vcad.app writes
    case foreign(String)      // a `vcad` entry the user owns — hands off
}

enum AgentInstall {
    static let serverName = "vcad"
    static let codexBegin = "# >>> vcad (managed by vcad.app — remove in vcad ▸ Settings ▸ Agents)"
    static let codexEnd = "# <<< vcad"

    /// The user's home. `VCAD_AGENT_HOME` points detection and writes at a
    /// sandbox directory instead (demos, screenshots, UI tests).
    static var home: URL {
        if let h = ProcessInfo.processInfo.environment["VCAD_AGENT_HOME"], !h.isEmpty {
            return URL(fileURLWithPath: h, isDirectory: true)
        }
        return FileManager.default.homeDirectoryForCurrentUser
    }

    // MARK: what to register

    /// The single swap point for the registered command. Today: the published
    /// npm bundle (`npx -y @vcad/mcp`). When the `vcad mcp` CLI shim (M0) ships
    /// inside vcad.app, point this at it and every agent follows on the next
    /// reconnect. Agents like Claude Desktop launch servers with a bare PATH,
    /// so `npx` is absolute and its `node` is put on PATH explicitly.
    static func serverEntry(npx: String) -> ServerEntry {
        let dir = (npx as NSString).deletingLastPathComponent
        return ServerEntry(command: npx, args: ["-y", "@vcad/mcp"],
                           env: ["PATH": "\(dir):/usr/bin:/bin:/usr/sbin:/sbin"])
    }

    /// Is this JSON/TOML-decoded entry one vcad.app wrote? Matches on shape,
    /// not exact paths, so a moved node install still counts as ours.
    static func isOurs(command: String?, args: [String]) -> Bool {
        guard let command else { return false }
        let base = (command as NSString).lastPathComponent
        return (base == "npx" && args == ["-y", "@vcad/mcp"]) || (base == "vcad" && args == ["mcp"])
    }

    /// The `vcad mcp` CLI (M0, muni-works/vcad#1099): attaches to the running
    /// app over ~/.vcad/vcad.sock and falls back to headless. Preferred over
    /// npx when present. A `vcad` on disk only counts if `vcad mcp --help`
    /// succeeds, so an older CLI without the subcommand is skipped.
    static func cliEntry(home: URL = AgentInstall.home) -> ServerEntry? {
        let fm = FileManager.default
        var candidates: [String] = []
        if let bundled = Bundle.main.url(forAuxiliaryExecutable: "vcad-cli")?.path { candidates.append(bundled) }
        candidates += ["/opt/homebrew/bin/vcad", "/usr/local/bin/vcad", "\(home.path)/.cargo/bin/vcad"]
        for c in candidates where fm.isExecutableFile(atPath: c) {
            let p = Process()
            p.executableURL = URL(fileURLWithPath: c)
            p.arguments = ["mcp", "--help"]
            p.standardOutput = FileHandle.nullDevice
            p.standardError = FileHandle.nullDevice
            guard (try? p.run()) != nil else { continue }
            p.waitUntilExit()
            if p.terminationStatus == 0 { return ServerEntry(command: c, args: ["mcp"], env: [:]) }
        }
        return nil
    }

    /// What to register: `vcad mcp` when available, else npx.
    static func preferredEntry() -> ServerEntry? {
        if let cli = cliEntry() { return cli }
        return npxPath().map { serverEntry(npx: $0) }
    }

    /// Absolute `npx`: common install locations, then the login shell's PATH.
    static func npxPath(home: URL = AgentInstall.home) -> String? {
        let fm = FileManager.default
        let candidates = ["/opt/homebrew/bin/npx", "/usr/local/bin/npx", "\(home.path)/.volta/bin/npx"]
        if let hit = candidates.first(where: { fm.isExecutableFile(atPath: $0) }) { return hit }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/zsh")
        p.arguments = ["-lc", "command -v npx"]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return nil }
        p.waitUntilExit()
        let s = String(decoding: out.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return fm.isExecutableFile(atPath: s) ? s : nil
    }

    // MARK: status

    static func status(_ agent: AgentKind, home: URL = AgentInstall.home) -> AgentStatus {
        let url = agent.config(home: home)
        if !agent.isDetected(home: home) && !FileManager.default.fileExists(atPath: url.path) { return .notDetected }
        switch agent {
        case .codex:
            guard let text = try? String(contentsOf: url, encoding: .utf8) else { return .available }
            if text.contains(codexBegin) { return .connected }
            if text.range(of: #"(?m)^\s*\[mcp_servers\.vcad\]"#, options: .regularExpression) != nil {
                return .foreign("a [mcp_servers.vcad] table you added")
            }
            return .available
        default:
            guard let root = try? readJSON(url),
                  let entry = (root["mcpServers"] as? [String: Any])?[serverName] as? [String: Any]
            else { return .available }
            let cmd = entry["command"] as? String
            let args = entry["args"] as? [String] ?? []
            if isOurs(command: cmd, args: args) { return .connected }
            return .foreign(([cmd ?? entry["url"] as? String ?? "?"] + args).joined(separator: " "))
        }
    }

    // MARK: install / uninstall

    enum Outcome: Equatable { case added, alreadySet, leftAlone(String) }

    @discardableResult
    static func install(_ agent: AgentKind, entry: ServerEntry, home: URL = AgentInstall.home) throws -> Outcome {
        switch status(agent, home: home) {
        case .connected: return .alreadySet
        case .foreign(let what): return .leftAlone(what)
        case .available, .notDetected: break
        }
        let url = agent.config(home: home)
        try backupOnce(url)
        switch agent {
        case .codex:
            var text = (try? String(contentsOf: url, encoding: .utf8)) ?? ""
            if !text.isEmpty && !text.hasSuffix("\n") { text += "\n" }
            if !text.isEmpty { text += "\n" }
            text += codexBlock(entry)
            try write(Data(text.utf8), to: url)
        default:
            var root = (try? readJSON(url)) ?? [:]
            var servers = root["mcpServers"] as? [String: Any] ?? [:]
            servers[serverName] = entry.json
            root["mcpServers"] = servers
            try write(try JSONSerialization.data(withJSONObject: root, options: [.prettyPrinted, .sortedKeys]), to: url)
        }
        if agent == .claudeCode { try? installSkill(home: home) }
        return .added
    }

    /// Remove what vcad.app wrote. Returns true if the file changed. A
    /// user-owned `vcad` entry is never touched.
    @discardableResult
    static func uninstall(_ agent: AgentKind, home: URL = AgentInstall.home) throws -> Bool {
        if agent == .claudeCode { removeSkill(home: home) }
        guard status(agent, home: home) == .connected else { return false }
        let url = agent.config(home: home)
        switch agent {
        case .codex:
            let text = try String(contentsOf: url, encoding: .utf8)
            guard let a = text.range(of: codexBegin),
                  let b = text.range(of: codexEnd, range: a.upperBound..<text.endIndex) else { return false }
            var lineEnd = b.upperBound
            if lineEnd < text.endIndex, text[lineEnd] == "\n" { lineEnd = text.index(after: lineEnd) }
            var out = String(text[..<a.lowerBound]) + String(text[lineEnd...])
            while out.hasSuffix("\n\n") { out.removeLast() }
            try write(Data(out.utf8), to: url)
        default:
            var root = try readJSON(url)
            var servers = root["mcpServers"] as? [String: Any] ?? [:]
            servers.removeValue(forKey: serverName)
            root["mcpServers"] = servers
            try write(try JSONSerialization.data(withJSONObject: root, options: [.prettyPrinted, .sortedKeys]), to: url)
        }
        return true
    }

    static func codexBlock(_ e: ServerEntry) -> String {
        func q(_ s: String) -> String {
            "\"" + s.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"") + "\""
        }
        var lines = [codexBegin, "[mcp_servers.vcad]", "command = \(q(e.command))",
                     "args = [\(e.args.map(q).joined(separator: ", "))]"]
        if !e.env.isEmpty {
            lines.append("env = { \(e.env.keys.sorted().map { "\($0) = \(q(e.env[$0]!))" }.joined(separator: ", ")) }")
        }
        lines.append(codexEnd)
        return lines.joined(separator: "\n") + "\n"
    }

    // MARK: skill (Claude Code)

    static let skillMarker = "<!-- installed by vcad.app; remove in vcad ▸ Settings ▸ Agents -->"

    static func skillURL(home: URL) -> URL { home.appendingPathComponent(".claude/skills/vcad/SKILL.md") }

    /// The skill bundled with this build: Resources for a shipped app, else the
    /// repo's plugin copy (swift run / tests). One file, three channels.
    static func bundledSkill() -> String? {
        if let p = Bundle.main.path(forResource: "SKILL", ofType: "md", inDirectory: "vcad-skill"),
           let s = try? String(contentsOfFile: p, encoding: .utf8) { return s }
        let repo = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("plugins/vcad/skills/vcad/SKILL.md")
        return try? String(contentsOf: repo, encoding: .utf8)
    }

    /// Install (or refresh) the skill. Never overwrites a SKILL.md the user wrote.
    static func installSkill(home: URL, skill: String? = AgentInstall.bundledSkill()) throws {
        guard let skill else { return }
        let url = skillURL(home: home)
        if let existing = try? String(contentsOf: url, encoding: .utf8), !existing.contains(skillMarker) { return }
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data((skill + "\n" + skillMarker + "\n").utf8).write(to: url, options: .atomic)
    }

    static func removeSkill(home: URL) {
        let url = skillURL(home: home)
        guard let existing = try? String(contentsOf: url, encoding: .utf8), existing.contains(skillMarker) else { return }
        try? FileManager.default.removeItem(at: url)
        try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) // only if now empty
    }

    // MARK: file plumbing

    static func readJSON(_ url: URL) throws -> [String: Any] {
        let data = try Data(contentsOf: url)
        if data.isEmpty { return [:] }
        guard let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw CocoaError(.fileReadCorruptFile, userInfo: [NSFilePathErrorKey: url.path])
        }
        return obj
    }

    static func backupOnce(_ url: URL) throws {
        let fm = FileManager.default
        guard let data = try? Data(contentsOf: url), !data.isEmpty else { return }
        let backup = url.appendingPathExtension("vcad-backup")
        if !fm.fileExists(atPath: backup.path) { try data.write(to: backup) }
    }

    static func write(_ data: Data, to url: URL) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try data.write(to: url, options: .atomic)
    }

    static func tilde(_ url: URL, home: URL = AgentInstall.home) -> String {
        url.path.replacingOccurrences(of: home.path, with: "~")
    }
}
