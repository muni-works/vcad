import Foundation

// Agent edits: the write half of the app's MCP server (AgentBridge.swift).
//
// A local Claude session authors the OPEN document, live: every call that
// changes geometry lands in the viewport immediately and is exactly one named
// undo step ("Undo Claude: Create Bracket"), so the user can walk an agent's
// build back one step at a time with ⌘Z.
//
// The vocabulary is the engine's own agent tool lowering (the same
// `create`/`update`/`delete`/`set_material`/booleans/transforms the built-in
// agent uses, from `crates/vcad-app/agent/manifest.json`), plus the
// `@vcad/mcp` surface that lowering doesn't cover: `create_cad_loon`,
// `set_parameters`, and the reads an author needs (`get_document`,
// `describe_scene`, `inspect_cad`, `list_parameters`).
//
// View-only documents (a generated `.loon` the file watch owns, electronics)
// refuse edits; an empty Untitled window gets an engine document on first use.

@MainActor
enum AgentEdits {
    typealias Failure = AgentTools.Failure

    /// Engine tools the bridge does NOT re-export: `render_view` is covered by
    /// the bridge's own `snapshot` of the real viewport.
    private static let skippedEngineTools: Set<String> = ["render_view"]

    /// Engine agent tools, as MCP tool definitions (`inputSchema`).
    static let engineTools: [[String: Any]] = {
        let m = DocumentEngine.agentManifest()
        let all = (m["tools"] as? [[String: Any]] ?? []) + (m["vision_tools"] as? [[String: Any]] ?? [])
        return all.compactMap { t in
            let f = t["function"] as? [String: Any] ?? t
            guard let name = f["name"] as? String, !skippedEngineTools.contains(name) else { return nil }
            var desc = f["description"] as? String ?? ""
            if !readEngineTools.contains(name) { desc += " Live in the user's viewport; one undo step." }
            return ["name": name, "description": desc,
                    "inputSchema": f["parameters"] ?? ["type": "object", "properties": [String: Any]()]]
        }
    }()
    static var engineToolNames: Set<String> { Set(engineTools.compactMap { $0["name"] as? String }) }
    static let readEngineTools: Set<String> = ["describe_scene", "inspect"]


    /// Link-handshake host tools this file implements (listed by the core).
    static let hostToolNames: Set<String> = ["adopt_document", "attach_document", "link_status"]

    /// Edit tools a `suggest_edit` pill may carry (run through the core on Add).
    static var suggestableTools: Set<String> {
        engineToolNames.subtracting(readEngineTools).union(["set_parameters", "create_cad_loon"])
    }

    // MARK: dispatch

    /// The host half of the link handshake (the rest of the edit vocabulary —
    /// engine tools, loon, parameters, undo, reads — is native in the MCP core,
    /// crates/vcad-mcp; engine edits come back here via `AgentCore`'s
    /// `engine_tool` so they run on this window's engine).
    static func call(_ name: String, _ a: [String: Any], model m: EditorModel) throws -> [String: Any] {
        switch name {
        case "adopt_document": return try adoptDocument(a, m)
        case "link_status": return try linkStatus(a, m)
        case "attach_document": return try attachDocument(a, m)
        default:
            throw Failure(message: "unknown tool \(name)")
        }
    }

    /// An engine read tool (`describe_scene`, `inspect`) on the window's engine.
    static func readEngine(_ m: EditorModel, _ tool: String, _ a: [String: Any]) throws -> String {
        try read(try editableEngine(m), tool, a)
    }

    // MARK: edits

    private static func editableEngine(_ m: EditorModel) throws -> DocumentEngine {
        guard let e = m.agentEnsureEngine() else {
            throw Failure(message: "The open document is view-only (\(m.documentName)). Open a .vcad file or a new Untitled window to edit.")
        }
        return e
    }

    /// One edit = one undo step named after it. `body` mutates the engine; a
    /// failure restores the snapshot so the document is exactly as before.
    @discardableResult
    static func edit(_ m: EditorModel, name: String, standalone: Bool = false,
                     _ body: (DocumentEngine) throws -> DocumentEngine.Result) throws -> DocumentEngine.Result {
        let engine = try editableEngine(m)
        let focus = AgentCursor.focus(m)
        // AgentTurn lands this as its own undo step, or folds it into the
        // agent's open turn ("Agent: thickened walls").
        let r = try AgentTurn.shared.perform(m, name: name, standalone: standalone) { () throws -> DocumentEngine.Result in
            let before = engine.save()
            let r: DocumentEngine.Result
            do {
                r = try body(engine)
            } catch {
                _ = try? engine.restore(before)
                throw Failure(message: describe(error))
            }
            m.agentApplied(r)
            return r
        }
        AgentCursor.shared.pointAt(touched: r, verb: AgentTurn.shared.undoName(fallback: name), focus: focus, model: m)
        return r
    }


    /// "Claude: Create Bracket", "Claude: Difference", "Claude: Fillet".
    static func undoName(_ tool: String, _ a: [String: Any]) -> String {
        let verb = tool.split(separator: "_").map { $0.capitalized }.joined(separator: " ")
        if tool == "create" {
            let label = (a["name"] as? String).flatMap { $0.isEmpty ? nil : $0 }
                ?? (a["type"] as? String)?.capitalized ?? "Part"
            return "Claude: Create \(label)"
        }
        if let n = a["name"] as? String, !n.isEmpty { return "Claude: \(verb) \(n)" }
        return "Claude: \(verb)"
    }

    private static func adoptDocument(_ a: [String: Any], _ m: EditorModel) throws -> [String: Any] {
        guard let document = a["document"] as? [String: Any], let link = a["link_id"] as? String else {
            throw Failure(message: "document and link_id are required")
        }
        let mine = m.agentLinkID == link
        let idle = m.agentLinkActiveAt.map { Date().timeIntervalSince($0) } ?? .infinity
        if !mine, let reason = takeoverRefusal(scratch: m.isDisposableScratch, agentsOnly: canTakeOver(m),
                                               intent: a["intent"] as? String, idleSeconds: idle) {
            return AgentTools.json(["adopted": false, "reason": reason])
        }
        if mine, try editableEngine(m).stateHash != m.agentLinkRevision {
            return AgentTools.json(["adopted": false, "reason": "edited"])
        }
        let tool = (a["tool"] as? String) ?? "edit"
        let oldBoard = m.electronics.board(m)
        try edit(m, name: "Agent: \(tool)") { try $0.loadIR(document) }
        if !oldBoard.isEmpty { m.electronics.revealNewCopper(from: oldBoard, to: m.electronics.board(m)) }
        let revision = try editableEngine(m).stateHash
        let displaced = mine ? nil : m.agentLinkID
        if !mine {
            // A new session's document: nothing of the last one's pose carries over.
            m.jointPose = [:]; m.explode = 0; m.agentBaseFirst = nil; m.agentBaseDelta = nil
            m.agentLinkOwned = true
            m.agentLinkBaseDir = nil
            m.agentPoseSurfacedLink = nil
        }
        if let dir = a["base_dir"] as? String, !dir.isEmpty { m.agentLinkBaseDir = dir }
        m.agentLinkID = link
        m.agentLinkRevision = revision
        m.agentLinkActiveAt = Date()
        // A mechanical document is blank in Electronics. Only on arrival or when
        // the board goes away, so a user who walks back there isn't pulled out.
        if m.workspace == .electronics, m.electronics.board(m).isEmpty, !mine || !oldBoard.isEmpty {
            GlassUI.shared.enter(.design, model: m)
        }
        var out: [String: Any] = ["adopted": true, "revision": revision]
        if let displaced { out["displaced"] = displaced }
        return AgentTools.json(out)
    }

    /// How long a linked session must be quiet before a deliberately opened one
    /// may take its window.
    static let takeoverIdleSeconds: TimeInterval = 30

    /// Why a push from another session may not replace this window, or nil if
    /// it may. An empty window is free. The user's work is never taken. A
    /// window showing another agent's build moves only on intent: an explicit
    /// call, or a deliberately opened session once the old one has gone idle.
    /// A session a tool minted implicitly never takes it.
    static func takeoverRefusal(scratch: Bool, agentsOnly: Bool, intent: String?, idleSeconds: TimeInterval) -> String? {
        if scratch { return nil }
        guard agentsOnly else { return "busy" }
        switch intent {
        case "explicit": return nil
        case "opened": return idleSeconds >= takeoverIdleSeconds ? nil : "linked_active"
        default: return "linked"
        }
    }

    /// Whether a different session may replace this window's document: it got
    /// here by an agent's push, is still in its unsaved Untitled window, and
    /// nobody has changed it since. Anything else is the user's.
    private static func canTakeOver(_ m: EditorModel) -> Bool {
        guard m.agentLinkID != nil, m.agentLinkOwned, m.source.isSandbox, let engine = m.engine else { return false }
        return engine.stateHash == m.agentLinkRevision
    }

    /// A pose arrived for a linked window that can't show one: go to Assembly,
    /// once per link, so a stream of poses never fights the user.
    static func surfacePose(_ m: EditorModel) {
        guard let link = m.agentLinkID, m.agentPoseSurfacedLink != link else { return }
        m.agentPoseSurfacedLink = link
        if m.workspace != .design || GlassUI.shared.space == .fab { GlassUI.shared.enter(.assembly, model: m) }
    }

    /// Hand the open document to a new MCP session. Editable documents become
    /// linked; the user's work stays theirs, so `agentLinkOwned` stays false and
    /// no other session can take the window. A generated `.loon` is owned by its
    /// file: the session gets a copy, and only poses mirror.
    private static func attachDocument(_ a: [String: Any], _ m: EditorModel) throws -> [String: Any] {
        guard let link = a["link_id"] as? String else { throw Failure(message: "link_id is required") }
        guard let document = m.documentJSON else { return AgentTools.json(["attached": false, "reason": "empty"]) }
        var out: [String: Any] = ["attached": true, "document": document, "name": m.documentName]
        if let path = m.documentURL?.path { out["path"] = path }
        if let dir = m.documentDirectory { out["base_dir"] = dir }
        guard let engine = m.engine, !m.isLoonDocument else {
            out["view_only"] = true
            return AgentTools.json(out)
        }
        m.agentLinkID = link
        m.agentLinkRevision = engine.stateHash
        m.agentLinkOwned = false
        m.agentLinkActiveAt = Date()
        m.agentPoseSurfacedLink = nil
        return AgentTools.json(out)
    }

    private static func linkStatus(_ a: [String: Any], _ m: EditorModel) throws -> [String: Any] {
        // `linked` is per session: @vcad/mcp reads false as "this session lost
        // the window", so a window held by someone else must still answer false.
        guard m.agentLinkID == (a["link_id"] as? String), m.agentLinkID != nil else {
            let reason = m.agentLinkID == nil ? "no_link" : (a["link_id"] == nil ? "no_link_id" : "other_session")
            return AgentTools.json(["linked": false, "window_linked": m.agentLinkID != nil, "reason": reason])
        }
        m.agentLinkActiveAt = Date()
        var out: [String: Any] = ["linked": true, "edited": try editableEngine(m).stateHash != m.agentLinkRevision]
        if a["pull"] as? Bool == true, let json = m.documentJSON {
            out["document"] = json
            m.agentLinkRevision = try editableEngine(m).stateHash
            m.agentLinkOwned = false   // the user's edits are in it now
        }
        return AgentTools.json(out)
    }


    /// Compile loon source to `.vcad` JSON through a temp file (the FFI takes
    /// a path so `[use]` resolves against a directory).
    static func compileLoon(_ source: String) throws -> [String: Any] {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("vcad-agent-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("main.loon")
        try Data(source.utf8).write(to: file)
        switch LoonCompiler.compile(path: file.path) {
        case .success(let data):
            guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                throw Failure(message: "loon compiled to something that isn't a document")
            }
            return json
        case .failure(let d):
            let at = [d.line.map { "line \($0)" }, d.column.map { "col \($0)" }].compactMap { $0 }.joined(separator: ", ")
            throw Failure(message: "loon error\(at.isEmpty ? "" : " (\(at))"): \(d.message)")
        }
    }

    // MARK: reads

    private static func read(_ engine: DocumentEngine, _ tool: String, _ a: [String: Any]) throws -> String {
        do { return try engine.agentRead(tool, a) } catch { throw Failure(message: describe(error)) }
    }


    static func parameters(_ m: EditorModel) -> [[String: Any]] {
        m.docParameters.map { p in
            var o: [String: Any] = ["name": p.name, "settable": p.isLiteral]
            if let v = p.value { o["value"] = v }
            if let lo = p.min { o["min"] = lo }
            if let hi = p.max { o["max"] = hi }
            if let u = p.unit { o["unit"] = u }
            return o
        }
    }


    static func describe(_ error: Error) -> String {
        if let r = error as? DocumentEngine.Refusal { return "\(r.kind): \(r.message)" }
        if let f = error as? Failure { return f.message }
        return String(describing: error)
    }
}
