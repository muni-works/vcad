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
         "description": "Show an MCP session's document in this window as one undo step. Sent by @vcad/mcp after each edit; agents don't call it. Refused when the window holds other work, or the user edited since the last push.",
         "inputSchema": schema(["document": ["type": "object"], "link_id": ["type": "string"],
                                "tool": ["type": "string"]], required: ["document", "link_id"])],
        ["name": "link_status",
         "description": "The current revision of a linked session's document, the document itself when pull is set.",
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
        guard mine || m.isDisposableScratch else { return AgentTools.json(["adopted": false, "reason": "busy"]) }
        if mine, try editableEngine(m).stateHash != m.agentLinkRevision {
            return AgentTools.json(["adopted": false, "reason": "edited"])
        }
        let tool = (a["tool"] as? String) ?? "edit"
        try edit(m, name: "Agent: \(tool)") { try $0.loadIR(document) }
        let revision = try editableEngine(m).stateHash
        m.agentLinkID = link
        m.agentLinkRevision = revision
        return AgentTools.json(["adopted": true, "revision": revision])
    }

    private static func linkStatus(_ a: [String: Any], _ m: EditorModel) throws -> [String: Any] {
        guard m.agentLinkID == (a["link_id"] as? String), m.agentLinkID != nil else {
            return AgentTools.json(["linked": false])
        }
        var out: [String: Any] = ["linked": true, "edited": try editableEngine(m).stateHash != m.agentLinkRevision]
        if a["pull"] as? Bool == true, let json = m.documentJSON {
            out["document"] = json
            m.agentLinkRevision = try editableEngine(m).stateHash
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
        let json = try compileLoon(source)
        let label = (a["name"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? "Loon Program"
        let r = try edit(m, name: "Claude: \(label)") { try $0.loadIR(json) }
        var text = "Loaded; document has \(r.partCount) part\(r.partCount == 1 ? "" : "s")"
        if !r.warnings.isEmpty { text += "; warnings: " + r.warnings.joined(separator: "; ") }
        return AgentTools.text(text + ".")
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
