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

    private static func schema(_ props: [String: Any], required: [String] = []) -> [String: Any] {
        ["type": "object", "properties": props, "required": required]
    }

    static let ownTools: [[String: Any]] = [
        ["name": "get_document",
         "description": "The open document's IR as JSON (nodes, roots, materials, parameters). Large documents are summarized unless full:true.",
         "inputSchema": schema(["full": ["type": "boolean"]])],
        ["name": "inspect_cad",
         "description": "Per-part volume (mm³), surface area (mm²), bounding box and mass (when the material density is known). Omit part_id for every part.",
         "inputSchema": schema(["part_id": ["type": "string"]])],
        ["name": "list_parameters",
         "description": "Document-level named parameters ([defparam …] in loon): value, unit, range, and whether set_parameters can change them (formulas can't).",
         "inputSchema": schema([:])],
        ["name": "set_parameters",
         "description": "Set named document parameters, e.g. {\"wall\": 3.5}. Values clamp to their declared range; every bound dimension updates. One undo step.",
         "inputSchema": schema(["values": ["type": "object", "additionalProperties": ["type": "number"]]],
                               required: ["values"])],
        ["name": "create_cad_loon",
         "description": "Replace the open document with a loon program (vcad's Lisp CAD language), e.g. [defparam t 5.0 :min 2 :max 12] [pub let plate [fillet 1.0 [cube 40.0 20.0 t]]]. Every [pub let …] becomes a part. Use [defparam name value] for parameters set_parameters can change later. Prefer create/update/… for incremental steps the user can undo one at a time; one undo step.",
         "inputSchema": schema(["source": ["type": "string", "description": "loon source"],
                                "name": ["type": "string", "description": "Short name for the undo step"]],
                               required: ["source"])],
        ["name": "adopt_document",
         "description": "Show an MCP session's document in this window as one undo step. Sent by @vcad/mcp after each edit; agents don't call it. A window showing another agent's unedited push moves only for intent \"explicit\", or \"opened\" once that session has been idle 30 s. Refused when the window holds the user's work, or the user edited since the last push.",
         "inputSchema": schema(["document": ["type": "object"], "link_id": ["type": "string"],
                                "tool": ["type": "string"],
                                "intent": ["type": "string", "description": "implicit (default), opened, or explicit: how far this push may displace another session."],
                                "base_dir": ["type": "string", "description": "Folder the document's relative mesh paths resolve from."]],
                               required: ["document", "link_id"])],
        ["name": "attach_document",
         "description": "Link an MCP session to the document open in this window and return it. Sent by @vcad/mcp's attach_app_document; agents don't call it.",
         "inputSchema": schema(["link_id": ["type": "string"]], required: ["link_id"])],
        ["name": "link_status",
         "description": "Whether the session named by link_id is the one this window is linked to, and if so whether the user has edited since its last push (the document itself when pull is set). linked:false says nothing about other sessions: window_linked reports whether the window has a link at all, reason why this session isn't it.",
         "inputSchema": schema(["link_id": ["type": "string"], "pull": ["type": "boolean"]], required: ["link_id"])],
        ["name": "undo",
         "description": "Undo the last edit in the app (yours or the user's), exactly like ⌘Z.",
         "inputSchema": schema([:])],
    ]

    static var definitions: [[String: Any]] { ownTools + engineTools }
    static var toolNames: Set<String> { Set(ownTools.compactMap { $0["name"] as? String }).union(engineToolNames) }

    // MARK: dispatch

    static func call(_ name: String, _ a: [String: Any], model m: EditorModel) throws -> [String: Any] {
        switch name {
        case "get_document": return try getDocument(a, m)
        case "inspect_cad": return try inspect(a, m)
        case "list_parameters": return AgentTools.json(parameters(m))
        case "set_parameters": return try setParameters(a, m)
        case "create_cad_loon": return try createLoon(a, m)
        case "adopt_document": return try adoptDocument(a, m)
        case "link_status": return try linkStatus(a, m)
        case "attach_document": return try attachDocument(a, m)
        case "undo":
            guard let label = m.undoActionName else { throw Failure(message: "Nothing to undo.") }
            m.undo()
            return AgentTools.text("Undid \(label).")
        case _ where readEngineTools.contains(name):
            let engine = try editableEngine(m)
            return AgentTools.text(try read(engine, name, a))
        case _ where engineToolNames.contains(name):
            return try engineTool(name, a, m)
        default:
            throw Failure(message: "unknown tool \(name)")
        }
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
    static func edit(_ m: EditorModel, name: String,
                     _ body: (DocumentEngine) throws -> DocumentEngine.Result) throws -> DocumentEngine.Result {
        let engine = try editableEngine(m)
        let before = engine.save()
        let r: DocumentEngine.Result
        do {
            r = try body(engine)
        } catch {
            _ = try? engine.restore(before)
            throw Failure(message: describe(error))
        }
        m.agentApplied(r)
        m.agentEndTurn(before: before, name: name)
        return r
    }

    private static func engineTool(_ name: String, _ a: [String: Any], _ m: EditorModel) throws -> [String: Any] {
        let typed = try JSONDecoder().decode([String: EngineJSON].self,
                                             from: JSONSerialization.data(withJSONObject: a))
        let r = try edit(m, name: undoName(name, a)) { try $0.applyTool(ToolCall(tool: name, args: typed)) }
        var text = "ok"
        if let id = r.createdFeatureId { text += "; created part \(id)" }
        text += "; document has \(r.partCount) part\(r.partCount == 1 ? "" : "s")"
        if !r.warnings.isEmpty { text += "; warnings: " + r.warnings.joined(separator: "; ") }
        text += ". Undo label: \"\(undoName(name, a))\"."
        return AgentTools.text(text)
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

    private static func setParameters(_ a: [String: Any], _ m: EditorModel) throws -> [String: Any] {
        guard let values = a["values"] as? [String: Any], !values.isEmpty else {
            throw Failure(message: "values is required, e.g. {\"wall\": 3}")
        }
        let params = Dictionary(m.docParameters.map { ($0.name, $0) }, uniquingKeysWith: { a, _ in a })
        var applied: [String: Double] = [:]
        var refused: [String] = []
        for (k, v) in values {
            guard let p = params[k], let x = (v as? NSNumber)?.doubleValue else { refused.append("\(k) (unknown)"); continue }
            guard p.isLiteral else { refused.append("\(k) (formula)"); continue }
            var c = x
            if let lo = p.min { c = max(lo, c) }
            if let hi = p.max { c = min(hi, c) }
            applied[k] = c
        }
        guard !applied.isEmpty else {
            throw Failure(message: "No settable parameter in \(refused.joined(separator: ", ")). Known: \(params.keys.sorted().joined(separator: ", "))")
        }
        guard var json = m.documentJSON else { throw Failure(message: "No document is open.") }
        for (k, v) in applied { DocEdit.setParameter(&json, name: k, value: v) }
        let name = applied.count == 1 ? "Claude: Set \(applied.keys.first!)" : "Claude: Set Parameters"
        try edit(m, name: name) { try $0.loadIR(json) }
        var msg = "Set " + applied.sorted { $0.key < $1.key }.map { "\($0.key)=\($0.value)" }.joined(separator: ", ")
        if !refused.isEmpty { msg += ". Refused: \(refused.joined(separator: ", "))" }
        return AgentTools.text(msg)
    }

    private static func createLoon(_ a: [String: Any], _ m: EditorModel) throws -> [String: Any] {
        guard let source = a["source"] as? String, !source.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw Failure(message: "source is required")
        }
        var json = try compileLoon(source)
        var kept: String?
        if let session = m.documentJSON { kept = try carryElectronics(from: session, into: &json) }
        let label = (a["name"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? "Loon Program"
        let r = try edit(m, name: "Claude: \(label)") { try $0.loadIR(json) }
        var text = "Loaded; document has \(r.partCount) part\(r.partCount == 1 ? "" : "s")"
        if let kept { text += "; \(kept)" }
        if !r.warnings.isEmpty { text += "; warnings: " + r.warnings.joined(separator: "; ") }
        return AgentTools.text(text + ".")
    }

    /// Carry the open document's board and schematic into a compiled loon
    /// document, as the MCP path does (`carryElectronics` in loon.ts): loading
    /// the program replaces the mechanical parts, not the electronics. A
    /// program that brings its own board can't be merged and is refused before
    /// anything changes. Returns a note, or nil when there was nothing to keep.
    static func carryElectronics(from session: [String: Any], into doc: inout [String: Any]) throws -> String? {
        let nodes = session["nodes"] as? [String: [String: Any]] ?? [:]
        let op = { (id: Int) in nodes[String(id)]?["op"] as? [String: Any] }
        let isBoard = { (n: [String: Any]) in (n["op"] as? [String: Any])?["type"] as? String == "PcbBoard" }
        let boardIds = nodes.values.filter(isBoard).compactMap { ($0["id"] as? NSNumber)?.intValue }
        let hasPcb = session["pcb"] != nil
        let schematic = session["schematic"]
        guard !boardIds.isEmpty || hasPcb || schematic != nil else { return nil }

        var target = doc["nodes"] as? [String: [String: Any]] ?? [:]
        let programHasBoard = target.values.contains(where: isBoard) || doc["pcb"] != nil
        if ((!boardIds.isEmpty || hasPcb) && programHasBoard) || (schematic != nil && doc["schematic"] != nil) {
            throw Failure(message: "create_cad_loon: this document already holds a board or schematic and the program "
                + "defines its own, so they can't be merged; nothing was changed. Remove them from the source.")
        }

        // After the engine materializes a document the scene root is the
        // Translate above the board; carry that whole chain.
        func chain(_ root: Int) -> [Int]? {
            var out: [Int] = []
            var id: Int? = root
            while let at = id, let o = op(at), !out.contains(at) {
                out.append(at)
                let type = o["type"] as? String
                if type == "PcbBoard" { return out }
                id = ["Translate", "Rotate", "Scale"].contains(type ?? "") ? (o["child"] as? NSNumber)?.intValue : nil
            }
            return nil
        }
        let rootEntries = session["roots"] as? [[String: Any]] ?? []
        let rooted = rootEntries.compactMap { e -> (entry: [String: Any], chain: [Int])? in
            guard let r = (e["root"] as? NSNumber)?.intValue, let c = chain(r) else { return nil }
            return (e, c)
        }
        var carried: [Int] = []
        for id in rooted.flatMap(\.chain) + boardIds where !carried.contains(id) { carried.append(id) }

        var next = (target.keys.compactMap(Int.init).max() ?? -1) + 1
        var remap: [Int: Int] = [:]
        for id in carried { remap[id] = next; next += 1 }
        var targetRoots = doc["roots"] as? [[String: Any]] ?? []
        var materials = doc["materials"] as? [String: Any] ?? [:]
        let sessionMaterials = session["materials"] as? [String: Any] ?? [:]
        func keepMaterial(_ key: String?) {
            if let key, materials[key] == nil, let m = sessionMaterials[key] { materials[key] = m }
        }
        for (entry, c) in rooted {
            var e = entry
            e["root"] = remap[c[0]]!
            targetRoots.append(e)
            keepMaterial(entry["material"] as? String)
        }
        let rootedBoards = Set(rooted.compactMap(\.chain.last))
        for id in boardIds where !rootedBoards.contains(id) {
            targetRoots.append(["root": remap[id]!, "material": "__pcb_fr4__"])
            keepMaterial("__pcb_fr4__")
        }
        var partIds = doc["partIds"] as? [String: Any] ?? [:]
        let sessionPartIds = session["partIds"] as? [String: Any] ?? [:]
        for old in carried {
            var node = nodes[String(old)]!
            let id = remap[old]!
            node["id"] = id
            if var o = node["op"] as? [String: Any], let child = (o["child"] as? NSNumber)?.intValue {
                o["child"] = remap[child]!
                node["op"] = o
            }
            target[String(id)] = node
            if let stable = sessionPartIds[String(old)] { partIds[String(id)] = stable }
        }
        doc["nodes"] = target
        doc["roots"] = targetRoots
        doc["materials"] = materials
        if !partIds.isEmpty { doc["partIds"] = partIds }
        if let pcb = session["pcb"] { doc["pcb"] = pcb }
        if let schematic { doc["schematic"] = schematic }

        let what = [boardIds.isEmpty && !hasPcb ? nil : "PCB board", schematic == nil ? nil : "schematic"]
            .compactMap { $0 }.joined(separator: " and ")
        return "kept the document's \(what)"
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

    private static func inspect(_ a: [String: Any], _ m: EditorModel) throws -> [String: Any] {
        let engine = try editableEngine(m)
        if let id = a["part_id"] as? String, !id.isEmpty {
            return AgentTools.text(try read(engine, "inspect", ["part_id": id]))
        }
        let ids = m.engineState?.parts.map(\.id) ?? []
        guard !ids.isEmpty else { return AgentTools.text("The document is empty.") }
        let blocks = ids.map { id in
            (try? read(engine, "inspect", ["part_id": id])).map { "\(id):\n\($0)" } ?? "\(id): (not measurable)"
        }
        return AgentTools.text(blocks.joined(separator: "\n\n"))
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

    private static func getDocument(_ a: [String: Any], _ m: EditorModel) throws -> [String: Any] {
        guard let doc = m.documentJSON else {
            return AgentTools.text("Empty document (\(m.documentName)). Create parts with create or create_cad_loon.")
        }
        let data = (try? JSONSerialization.data(withJSONObject: doc, options: [.sortedKeys])) ?? Data()
        if a["full"] as? Bool == true || data.count < 60_000 {
            return AgentTools.text(String(decoding: data, as: UTF8.self))
        }
        var summary: [String: Any] = [:]
        for (k, v) in doc {
            if let d = v as? [String: Any] { summary[k] = "\(d.count) entries" }
            else if let l = v as? [Any] { summary[k] = "\(l.count) items" }
            else { summary[k] = v }
        }
        summary["roots"] = doc["roots"]
        summary["note"] = "Document is \(data.count) bytes; pass full:true for all of it."
        return AgentTools.json(summary)
    }

    static func describe(_ error: Error) -> String {
        if let r = error as? DocumentEngine.Refusal { return "\(r.kind): \(r.message)" }
        if let f = error as? Failure { return f.message }
        return String(describing: error)
    }
}
