#if os(macOS)
import AppKit
import SwiftUI

// The consent dialog (first launch, or Help ▸ Connect Agents…) and the
// Settings ▸ Agents pane that undoes it. Logic lives in AgentInstall.swift.

enum AgentInstallPrompt {
    static let promptedKey = "agentInstall.prompted"

    /// First launch: ask once. Skipped when no agent is installed (nothing to
    /// offer), when already asked, or under VCAD_SKIP_AGENT_PROMPT=1.
    @MainActor
    static func runIfFirstLaunch() {
        let env = ProcessInfo.processInfo.environment
        if env["VCAD_SKIP_AGENT_PROMPT"] == "1" { return }
        let force = env["VCAD_AGENT_PROMPT"] == "1"
        if !force && UserDefaults.standard.bool(forKey: promptedKey) { return }
        let offer = AgentKind.allCases.filter { AgentInstall.status($0, home: AgentInstall.home) == .available }
        if offer.isEmpty && !force { return }
        if !force { UserDefaults.standard.set(true, forKey: promptedKey) }
        // Let the editor window come up first so the sheet has context.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.8) { run() }
    }

    /// Show the one consent dialog listing every detected agent, then write
    /// the configs the user left checked.
    @MainActor
    static func run() {
        let home = AgentInstall.home
        let rows = AgentKind.allCases.map { ($0, AgentInstall.status($0, home: home)) }
            .filter { $0.1 != .notDetected }

        let alert = NSAlert()
        alert.messageText = "Connect vcad to your agents?"
        guard !rows.isEmpty else {
            alert.informativeText = "No supported agents found (Claude Code, Claude Desktop, Codex, Cursor). Install one, then use Help ▸ Connect Agents…. You can also add vcad by hand: npx -y @vcad/mcp"
            alert.runModal()
            return
        }
        alert.informativeText = "vcad will add itself as an MCP server, so you can ask your agent to model something and watch it build here. vcad runs on this Mac only — nothing is sent anywhere. Undo any time in Settings ▸ Agents."

        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 8
        var boxes: [(AgentKind, NSButton)] = []
        for (agent, status) in rows {
            let box = NSButton(checkboxWithTitle: agent.label, target: nil, action: nil)
            box.font = .systemFont(ofSize: NSFont.systemFontSize, weight: .medium)
            let detail: String
            switch status {
            case .available:
                box.state = .on
                detail = "adds “vcad” to \(AgentInstall.tilde(agent.config(home: home)))"
                    + (agent == .claudeCode ? " + the vcad skill" : "")
            case .connected:
                box.state = .on; box.isEnabled = false
                detail = "already connected"
            case .foreign(let what):
                box.state = .off; box.isEnabled = false
                detail = "has its own vcad server (\(what)) — left alone"
            case .notDetected:
                continue
            }
            let sub = NSTextField(labelWithString: detail)
            sub.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            sub.textColor = .secondaryLabelColor
            sub.lineBreakMode = .byTruncatingMiddle
            sub.widthAnchor.constraint(lessThanOrEqualToConstant: 380).isActive = true
            let row = NSStackView(views: [box, sub])
            row.orientation = .vertical
            row.alignment = .leading
            row.spacing = 1
            stack.addArrangedSubview(row)
            if status == .available { boxes.append((agent, box)) }
        }
        stack.frame = NSRect(x: 0, y: 0, width: 400, height: stack.fittingSize.height)
        alert.accessoryView = stack
        alert.addButton(withTitle: boxes.isEmpty ? "OK" : "Connect")
        if !boxes.isEmpty { alert.addButton(withTitle: "Not Now") }

        let response = alert.runModal()
        FileHandle.standardError.write(Data("[agent-install] consent response \(response.rawValue)\n".utf8))
        guard response == .alertFirstButtonReturn else { return }
        let chosen = boxes.filter { $0.1.state == .on }.map(\.0)
        guard !chosen.isEmpty else { return }
        connect(chosen)
    }

    /// Write the configs and report. Used by the dialog and by Settings.
    @MainActor
    @discardableResult
    static func connect(_ agents: [AgentKind], quiet: Bool = false) -> [String] {
        guard let entry = AgentInstall.preferredEntry() else {
            report(["vcad's agent server runs through Node.js, which isn't installed. Install it from nodejs.org or with `brew install node`, then use Help ▸ Connect Agents…."], ok: false)
            return []
        }
        var lines: [String] = []
        var ok = true
        for a in agents {
            do {
                switch try AgentInstall.install(a, entry: entry) {
                case .added: lines.append("✓ \(a.label): connected")
                case .alreadySet: lines.append("✓ \(a.label): already connected")
                case .leftAlone(let what): lines.append("– \(a.label): kept your own vcad server (\(what))")
                }
            } catch {
                ok = false
                lines.append("✗ \(a.label): couldn't write \(a.config(home: AgentInstall.home).lastPathComponent) — \(error.localizedDescription)")
            }
        }
        lines.append("\nRestart the agent (or start a new session) and ask it to model something.")
        if !quiet { report(lines, ok: ok) }
        return lines
    }

    @MainActor
    private static func report(_ lines: [String], ok: Bool) {
        let a = NSAlert()
        a.messageText = ok ? "vcad is connected" : "Some agents couldn't be connected"
        a.informativeText = lines.joined(separator: "\n")
        a.alertStyle = ok ? .informational : .warning
        a.runModal()
    }
}

/// Settings ▸ Agents: per-agent status with Connect / Disconnect.
struct AgentConnectionsForm: View {
    @State private var statuses: [AgentKind: AgentStatus] = [:]
    @State private var error: String?

    var body: some View {
        Section {
            ForEach(AgentKind.allCases) { agent in
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(agent.label)
                        Text(caption(agent)).font(.caption).foregroundStyle(.secondary)
                            .lineLimit(1).truncationMode(.middle)
                    }
                    Spacer()
                    switch statuses[agent] ?? .notDetected {
                    case .connected:
                        Button("Disconnect") { act { try AgentInstall.uninstall(agent) } }
                    case .available:
                        Button("Connect") { AgentInstallPrompt.connect([agent], quiet: true); refresh() }
                    case .foreign, .notDetected:
                        EmptyView()
                    }
                }
            }
            HStack {
                Spacer()
                Button("Disconnect All") {
                    act { for a in AgentKind.allCases { try AgentInstall.uninstall(a) } }
                }
                .disabled(!statuses.values.contains(.connected))
            }
            if let error { Label(error, systemImage: "exclamationmark.triangle").font(.caption).foregroundStyle(.orange) }
        } header: {
            Text("Agents using vcad")
        } footer: {
            Text("vcad registers itself as an MCP server (`vcad mcp` when installed, else npx -y @vcad/mcp) in each agent's config. Disconnect removes only what vcad added; a vcad entry you wrote yourself is never touched. Each file is backed up once to <file>.vcad-backup.")
                .font(.caption).foregroundStyle(.secondary)
        }
        .onAppear(perform: refresh)
    }

    private func caption(_ a: AgentKind) -> String {
        switch statuses[a] ?? .notDetected {
        case .notDetected: "Not installed"
        case .available: "Not connected · \(AgentInstall.tilde(a.config(home: AgentInstall.home)))"
        case .connected: "Connected · \(AgentInstall.tilde(a.config(home: AgentInstall.home)))"
        case .foreign(let what): "Uses your own vcad server: \(what)"
        }
    }

    private func refresh() {
        statuses = Dictionary(uniqueKeysWithValues: AgentKind.allCases.map { ($0, AgentInstall.status($0)) })
    }

    private func act(_ f: () throws -> Void) {
        do { try f(); error = nil } catch { self.error = error.localizedDescription }
        refresh()
    }
}
#endif
