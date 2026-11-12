import CVcadFFI
import Foundation
import RealityKit

// vcad.app hosts the Rust MCP core (crates/vcad-mcp, linked through vcad-ffi's
// mcp.rs): one dispatcher and one tool implementation, the same code
// `vcad mcp` runs headless. The core owns the protocol (both eras), the
// HTTP endpoint on 127.0.0.1, the tool list, the native tools (document
// edits, loon, parameters, receipts, fab/optimize tasks) and the node proxy
// for tools not yet ported.
//
// This file is the app's half: it answers the core's callback. Everything
// only the app can do lands here —
//
//   read / apply   the live document; each apply is ONE named undo step
//                  ("Claude: Create Bracket") the user watches land
//   engine_tool    an engine edit on the window's own engine (same Rust
//                  lowering), so feature history and the agent cursor stay
//   undo           ⌘Z
//   open_path      open a file in the window
//   resources      vcad://doc/current | selection | view, the viewer
//   host_call      host tools: camera, pose, highlights, snapshots,
//                  elicitation answered in the viewport, the @vcad/mcp link
//
// The callback runs on the core's worker threads; each request hops to the
// main actor and the worker waits for the answer.

/// The id the core uses for the window's document. The core hands agents
/// `app_window#<epoch>` (see `AgentCore.documentId`): pinned to the document
/// the window showed at the time, so an agent can only edit the document it
/// was pointed at. Bare `app_window` means "whatever the window shows now".
let agentWindowDocumentId = "app_window"

@MainActor
final class AgentCore {
    static let shared = AgentCore()

    weak var model: EditorModel?
    private(set) var app: OpaquePointer?
    private(set) var port: UInt16?

    /// Start the core over `model`. `serveHTTP` binds 127.0.0.1 (first free
    /// port from `port`, `attempts` tries); off for in-process use (tests).
    /// Returns the bound port, or nil when not serving / no port was free.
    @discardableResult
    func start(model: EditorModel, serveHTTP: Bool = true, port: UInt16 = AgentBridge.basePort,
               attempts: Int = 8, proxy: Bool = true) -> UInt16? {
        self.model = model
        guard app == nil else { return self.port }
        var cfg: [String: Any] = [
            "name": "vcad-app", "version": AgentCore.version, "instructions": AgentBridge.instructions,
            "serve_http": serveHTTP, "port": Int(port), "attempts": attempts, "proxy": proxy,
            "viewer_uri": AgentResources.viewerURI,
        ]
        if proxy, let cmd = AgentCore.nodeCommand() { cfg["node_cmd"] = cmd }
        let json = String(decoding: (try? JSONSerialization.data(withJSONObject: cfg)) ?? Data(), as: UTF8.self)
        guard let h = json.withCString({ vcad_mcp_app_start(agentCoreHost, nil, $0) }) else {
            FileHandle.standardError.write(Data("[vcad] MCP core failed to start: no free loopback port from \(port)\n".utf8))
            return nil
        }
        app = h
        let p = vcad_mcp_app_port(h)
        self.port = p == 0 ? nil : p
        return self.port
    }

    /// For tests: an in-process core (no HTTP, no node) over `model`.
    func attach(_ model: EditorModel) {
        self.model = model
        if app == nil { start(model: model, serveHTTP: false, proxy: false) }
        AgentResources.shared.store = AppStore(model: model)
    }

    /// The core's host-tool definitions (crates/vcad-mcp/src/tools/host_tools.json)
    /// — the single source for what the tools above are called and take.
    static let hostDefinitions: [[String: Any]] = {
        guard let raw = vcad_mcp_host_tools() else { return [] }
        defer { vcad_string_free(raw) }
        let text = String(cString: raw)
        return (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [[String: Any]] ?? []
    }()

    static var version: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev"
    }

    /// `npx -y @vcad/mcp` for the proxy, found where a GUI app's PATH misses it.
    /// `VCAD_MCP_NODE_CMD` wins (and `off` disables the proxy).
    static func nodeCommand() -> String? {
        let env = ProcessInfo.processInfo.environment
        if let c = env["VCAD_MCP_NODE_CMD"] { return c }
        for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] where
            FileManager.default.isExecutableFile(atPath: "\(dir)/npx") {
            return "\(dir)/npx -y @vcad/mcp"
        }
        return nil
    }

    // MARK: calls through the core

    /// One tool call through the core — exactly what an agent's `tools/call`
    /// does. Runs the core off the main actor (it calls back into it).
    func call(_ name: String, _ args: [String: Any] = [:], inputResponses: [String: Any]? = nil) async -> [String: Any] {
        var params: [String: Any] = ["name": name, "arguments": args]
        if let inputResponses { params["inputResponses"] = inputResponses }
        let r = await rpc("tools/call", params)
        if let result = r["result"] as? [String: Any] { return result }
        let msg = ((r["error"] as? [String: Any])?["message"] as? String) ?? "no result"
        return AgentTools.error(msg)
    }

    /// One JSON-RPC request through the core; the whole response.
    func rpc(_ method: String, _ params: [String: Any] = [:]) async -> [String: Any] {
        guard let app else { return ["error": ["code": -32603, "message": "MCP core not started"]] }
        let msg: [String: Any] = ["jsonrpc": "2.0", "id": 1, "method": method, "params": params]
        let body = String(decoding: (try? JSONSerialization.data(withJSONObject: msg)) ?? Data(), as: UTF8.self)
        let handle = SendablePointer(app)
        let text: String = await Task.detached {
            guard let raw = body.withCString({ vcad_mcp_app_call(handle.pointer, $0) }) else { return "{}" }
            defer { vcad_string_free(raw) }
            return String(cString: raw)
        }.value
        return (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any] ?? [:]
    }

    func resourceUpdated(_ uri: String) {
        guard let app else { return }
        uri.withCString { vcad_mcp_app_resource_updated(app, $0) }
    }

    // MARK: the core's callback

    /// Blocking bridge from a core worker thread to the main actor.
    nonisolated static func reply(_ request: String) -> String {
        final class Box: @unchecked Sendable { var text = "{}" }
        if Thread.isMainThread {
            // The core never calls back on the caller's thread; refuse rather
            // than deadlock if that contract is ever broken.
            return #"{"error":"vcad.app: MCP core called back on the main thread","kind":"unavailable"}"#
        }
        let box = Box()
        let done = DispatchSemaphore(value: 0)
        Task { @MainActor in
            box.text = await AgentCore.shared.dispatch(request)
            done.signal()
        }
        done.wait()
        return box.text
    }

    private func dispatch(_ request: String) async -> String {
        let req = (try? JSONSerialization.jsonObject(with: Data(request.utf8))) as? [String: Any] ?? [:]
        let out: [String: Any]
        do {
            out = try await answer(req["op"] as? String ?? "", req)
        } catch let e as AgentTools.Failure {
            out = ["error": e.message]
        } catch {
            out = ["error": AgentEdits.describe(error)]
        }
        return Self.encode(out)
    }

    /// Key under which `answer` hands back the document as ready-made JSON
    /// bytes (the runtime frame's, serialized off main), spliced into the
    /// reply as `"document"` instead of re-serializing a dictionary on main.
    nonisolated static let rawDocumentKey = "__raw_document"

    nonisolated static func encode(_ out: [String: Any]) -> String {
        var rest = out
        guard let raw = rest.removeValue(forKey: rawDocumentKey) as? Data else {
            return String(decoding: (try? JSONSerialization.data(withJSONObject: out)) ?? Data("{}".utf8), as: UTF8.self)
        }
        var text = String(decoding: (try? JSONSerialization.data(withJSONObject: rest)) ?? Data("{}".utf8), as: UTF8.self)
        let doc = String(decoding: raw, as: UTF8.self)
        text.removeLast()                                   // the closing brace
        return text + (rest.isEmpty ? "" : ",") + "\"document\":" + doc + "}"
    }

    /// The document for a reply: frame bytes when the runtime has them.
    private static func documentReply(_ m: EditorModel) -> (String, Any) {
        if m.documentJSON != nil, let data = m.pickingDocumentData { return (rawDocumentKey, data) }
        return ("document", m.documentJSON ?? emptyDocument)
    }

    /// An empty IR document (an Untitled window before its first part).
    static let emptyDocument: [String: Any] = ["version": "0.1", "nodes": [String: Any](), "roots": [Any]()]

    private func answer(_ op: String, _ r: [String: Any]) async throws -> [String: Any] {
        if op == "instance_meta" { return AgentBridge.shared.instanceMeta }
        guard let m = model else { return ["error": "The app has no window yet.", "kind": "unavailable"] }
        let id = r["id"] as? String
        AgentTurn.shared.epochProvider = { [weak m] in m?.documentEpoch ?? 0 }
        func mine() throws {
            if let refusal = Self.scopeRefusal(id, m) { throw AgentTools.Failure(message: refusal) }
        }
        switch op {
        case "front":
            let (k, v) = Self.documentReply(m)
            return ["id": Self.documentId(m), k: v]
        case "read":
            if let refusal = Self.scopeRefusal(id, m) {
                return ["error": refusal, "kind": Self.isStale(id, m) ? "scope" : "not_found"]
            }
            let (k, v) = Self.documentReply(m)
            return [k: v]
        case "documents":
            return ["documents": [["id": Self.documentId(m), "name": m.documentName]]]
        case "open", "apply":
            if op == "apply" { try mine() }
            guard let doc = r["document"] as? [String: Any] else { throw AgentTools.Failure(message: "document is required") }
            if let busy = AgentGhost.shared.pending { throw Self.busy(busy) }
            let label = (r["label"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? "Edit"
            let name = "\(AgentCore.agentPrefix): \(label)"
            let oldBoard = m.electronics.board(m)
            let before = Self.partIds(m)
            let gated = op == "apply" && AgentGhost.shared.enabled && m.agentCanEdit && !before.isEmpty
            let ghost = gated ? AgentGhost.capture(parts: Array(0..<m.partCount)) : nil
            let depth = m.undoStack.count
            try AgentEdits.edit(m, name: name, standalone: gated) { try $0.loadIR(doc) }
            if !oldBoard.isEmpty { m.electronics.revealNewCopper(from: oldBoard, to: m.electronics.board(m)) }
            if gated, let why = AgentGhost.shared.reason(before: before, after: Self.partIds(m), wholeDocument: true) {
                try await Self.gate(m, name: name, why: why, ghost: Self.keepOnly(ghost, removed: before, after: Self.partIds(m), m), depth: depth)
            }
            return op == "open" ? ["id": Self.documentId(m)] : [:]
        case "release":
            return [:]
        case "job":
            AgentJobs.shared.handle(r["job"] as? [String: Any] ?? [:], progress: m.progress)
            return [:]
        case "undo":
            try mine()
            // ⌘Z mid-sketch is scoped to the sketch's own history. An agent's
            // undo means the document, so refuse rather than silently eat the
            // user's sketch strokes (or rewind the document under the sketch).
            if m.sketchHistory != nil {
                return ["error": "A sketch is open in the app, and undo there acts on the sketch, not the document. Nothing was undone — ask the user to finish or cancel the sketch, then undo again."]
            }
            guard let label = m.undoActionName else { return ["error": "Nothing to undo"] }
            m.undo()
            return ["label": label]
        case "open_path":
            guard let path = r["path"] as? String else { throw AgentTools.Failure(message: "path is required") }
            // Fail closed on anything that isn't an existing document: an
            // arbitrary file (e.g. /etc/passwd, a user's notes.txt) would
            // otherwise become the window's document — and its Save target.
            guard let url = AgentCore.openableDocument(path) else {
                throw AgentTools.Failure(message: "not an openable vcad document (need an existing .vcad, .loon, .dxf, .step/.stp, .urdf file or a loon folder): \(path)")
            }
            m.openDocument(url)
            return ["id": agentWindowDocumentId]
        case "live_resources":
            return ["resources": AgentResources.shared.list()["resources"] ?? []]
        case "read_resource":
            let uri = r["uri"] as? String ?? ""
            guard uri == AgentResources.viewerURI || StoreResource.parse(uri) != nil else { return [:] }
            if AgentResources.shared.store == nil { AgentResources.shared.store = AppStore(model: m) }
            return ["result": try AgentResources.shared.read(uri)]
        case "engine_tool":
            try mine()
            return ["envelope": await engineTool(r["tool"] as? String ?? "", r["args"] as? [String: Any] ?? [:], m)]
        case "host_call":
            let name = r["name"] as? String ?? ""
            let args = r["args"] as? [String: Any] ?? [:]
            if name == "__client_hello" {
                AgentCursor.shared.agentName = AgentBridge.displayName(args["name"] as? String ?? "")
                return ["result": AgentTools.text("ok")]
            }
            if let refusal = Self.scopeRefusal(args["document_id"] as? String, m) {
                return ["result": AgentTools.error(refusal)]
            }
            let inputs = r["input_responses"] as? [String: Any]
            return ["result": await hostCall(name, args, inputs, m)]
        default:
            return ["error": "unknown op \(op)", "kind": "unavailable"]
        }
    }

    /// Undo-step prefix: the connected agent's name ("Claude: Create Bracket").
    static var agentPrefix: String {
        let n = AgentCursor.shared.agentName
        return n == "Agent" ? "Claude" : n
    }

    /// An engine tool on the window's engine — the same `vcad_app` lowering the
    /// core runs headless — as one named undo step with the agent cursor.
    private func engineTool(_ tool: String, _ raw: [String: Any], _ m: EditorModel) async -> [String: Any] {
        func refusal(_ e: Error) -> [String: Any] {
            if let r = e as? DocumentEngine.Refusal { return ["ok": false, "error": ["kind": r.kind, "message": r.message]] }
            if let f = e as? AgentTools.Failure { return ["ok": false, "error": ["kind": "invalid", "message": f.message]] }
            return ["ok": false, "error": ["kind": "invalid", "message": AgentEdits.describe(e)]]
        }
        do {
            // "this part" / "these parts": selection references → part ids.
            let a = try AgentSelection.resolve(raw, m)
            if AgentEdits.readEngineTools.contains(tool) {
                return ["ok": true, "result": ["text": try AgentEdits.readEngine(m, tool, a)]]
            }
            if let busy = AgentGhost.shared.pending { throw Self.busy(busy) }
            let typed = try JSONDecoder().decode([String: EngineJSON].self, from: JSONSerialization.data(withJSONObject: a))
            let name = AgentEdits.undoName(tool, a)
            // The cursor goes where the tool is about to act, first.
            await AgentCursor.shared.leadIn(to: AgentSelection.target(a, m), verb: name)
            let ghost = AgentGhost.shared
            let gated = ghost.enabled && ghost.touchesUserParts(tool, a)
            let before = Self.partIds(m)
            let shot = gated ? AgentGhost.capture(parts: Array(0..<m.partCount)) : nil
            let depth = m.undoStack.count
            let r = try AgentEdits.edit(m, name: name, standalone: gated) {
                try $0.applyTool(ToolCall(tool: tool, args: typed))
            }
            if let c = r.createdFeatureId { ghost.agentCreated.insert(c) }
            if gated, let why = ghost.reason(before: before, after: Self.partIds(m)) {
                try await Self.gate(m, name: name, why: why,
                                    ghost: Self.keepOnly(shot, removed: before, after: Self.partIds(m), m), depth: depth)
            }
            var result: [String: Any] = ["parts": r.parts.map { _ in [String: Any]() }, "warnings": r.warnings]
            if let c = r.createdFeatureId { result["createdFeatureId"] = c }
            return ["ok": true, "result": result]
        } catch {
            return refusal(error)
        }
    }

    /// Host tools (Swift implementations) plus the destructive-edit confirm.
    private func hostCall(_ name: String, _ a: [String: Any], _ inputs: [String: Any]?, _ m: EditorModel) async -> [String: Any] {
        do {
            if name == "__confirm_destructive" {
                var given = ProofAgentTools.InputResponses(inputs)
                let req = ElicitRequest(message: a["message"] as? String ?? "Confirm a destructive edit?",
                                        schema: ["type": "object", "properties": [String: Any]()], destructive: true)
                let resp = await ProofAgentTools.elicit(req, &given, model: m)
                return ["action": resp.action.rawValue]
            }
            if ProofAgentTools.toolNames.contains(name) {
                return try await ProofAgentTools.call(name, a, inputResponses: inputs, model: m)
            }
            return try await AgentTools.call(name, a, model: m)
        } catch let e as AgentTools.Failure {
            return AgentTools.error(e.message)
        } catch {
            return AgentTools.error("\(error)")
        }
    }
}

/// The core's callback (C ABI): JSON request in, `strdup`ed JSON reply out.
private let agentCoreHost: vcad_mcp_host_fn = { _, request in
    let text = request.map { String(cString: $0) } ?? "{}"
    return strdup(AgentCore.reply(text))
}

/// A raw pointer handed to a detached task (the core is thread-safe).
private struct SendablePointer: @unchecked Sendable {
    let pointer: OpaquePointer
    init(_ p: OpaquePointer) { pointer = p }
}

// MARK: - scope, selection refs, ghost gate

extension AgentCore {
    /// The window document's pinned id: `app_window#<epoch>`.
    static func documentId(_ m: EditorModel) -> String { "\(agentWindowDocumentId)#\(m.documentEpoch)" }

    static func isStale(_ id: String?, _ m: EditorModel) -> Bool {
        guard let id else { return false }
        return id.hasPrefix(agentWindowDocumentId + "#") && id != documentId(m)
    }

    /// Nil when `id` names the window's current document (or is omitted /
    /// bare `app_window`); else why the call must not touch it.
    static func scopeRefusal(_ id: String?, _ m: EditorModel) -> String? {
        guard let id, id != agentWindowDocumentId, id != documentId(m) else { return nil }
        if isStale(id, m) {
            return "SCOPE: document_id '\(id)' is a document this window no longer shows — the user switched it to \(m.documentName) (\(documentId(m))). Nothing was changed: this agent was pointed at the earlier document. Ask the user before working on the new one."
        }
        return "Unknown document_id '\(id)' — this window's document is \(documentId(m)) (or omit document_id)."
    }

    static func partIds(_ m: EditorModel) -> [String] {
        (0..<m.partCount).compactMap { m.partId(forPart: $0) }
    }

    static func busy(_ p: AgentGhost.Pending) -> AgentTools.Failure {
        AgentTools.Failure(message: "The user is still reviewing your previous edit (\(p.label)) in the app. Wait for that answer before editing again.")
    }

    /// Keep only the ghost clones of parts the edit removed.
    static func keepOnly(_ ghost: Entity?, removed before: [String], after: [String], _ m: EditorModel) -> Entity? {
        guard let ghost else { return nil }
        for (i, id) in before.enumerated() where after.contains(id) {
            ghost.findEntity(named: "ghost\(i)")?.removeFromParent()
        }
        return ghost.children.isEmpty ? nil : ghost
    }

    /// Hold the agent's call on the user's Apply / Reject. Reject undoes
    /// exactly this edit (its own undo entry) and fails the call.
    static func gate(_ m: EditorModel, name: String, why: String, ghost: Entity?, depth: Int) async throws {
        let label = name.replacingOccurrences(of: "Claude: ", with: "\(AgentCursor.shared.agentName) wants to: ")
        let ok = await AgentGhost.shared.review(.init(label: label, summary: why), ghost: ghost)
        if ok { return }
        guard m.undoStack.count == depth + 1, m.undoActionName == name || m.undoActionName == AgentTurn.shared.undoName(fallback: name) else {
            throw AgentTools.Failure(message: "The user rejected \"\(name)\" (\(why)), but edited the document while reviewing, so it could not be rolled back automatically — ask them.")
        }
        m.undo()
        throw AgentTools.Failure(message: "The user rejected \"\(name)\" in the app (\(why)). The document is unchanged. Ask what they want instead of retrying.")
    }
}

/// Selection references in tool arguments: "this part", "these parts".
///
/// Any part-valued argument may be `"selection"` (also `"$selection"`,
/// `"vcad://doc/selection"`): it resolves to the part the user has selected
/// in the viewport — or, in an array (`part_ids`), to every selected part.
@MainActor
enum AgentSelection {
    static let refs: Set<String> = ["selection", "$selection", "@selection", "vcad://doc/selection"]
    static let partKeys: Set<String> = ["part_id", "child", "left", "right", "node_id", "parent_part_id", "part"]

    static func selectedParts(_ m: EditorModel) -> [Int] {
        if !m.multiSelectedParts.isEmpty { return m.multiSelectedParts.sorted() }
        if let i = m.selectedPartIndex ?? m.picking.selectionPart { return [i] }
        return []
    }

    static func resolve(_ a: [String: Any], _ m: EditorModel) throws -> [String: Any] {
        var out = a
        func ids() throws -> [String] {
            let ids = selectedParts(m).compactMap { m.partId(forPart: $0) }
            guard !ids.isEmpty else {
                throw AgentTools.Failure(message: "\"selection\" was passed, but the user has nothing selected in the viewport. Ask them to click the part, or name it (describe_scene lists ids).")
            }
            return ids
        }
        for (k, v) in a {
            if let s = v as? String, refs.contains(s), partKeys.contains(k) {
                let all = try ids()
                guard all.count == 1 else {
                    throw AgentTools.Failure(message: "\"selection\" for \(k) is ambiguous: the user has \(all.count) parts selected (\(all.joined(separator: ", "))).")
                }
                out[k] = all[0]
            } else if let arr = v as? [Any], arr.contains(where: { ($0 as? String).map(refs.contains) ?? false }) {
                var expanded: [Any] = []
                for x in arr {
                    if let s = x as? String, refs.contains(s) { expanded += try ids() } else { expanded.append(x) }
                }
                out[k] = expanded
            }
        }
        return out
    }

    /// Where an edit with these (resolved) args is about to act.
    static func target(_ a: [String: Any], _ m: EditorModel) -> AgentCursorTarget? {
        let focus = AgentCursor.focus(m)
        for k in ["part_id", "child", "left", "node_id", "parent_part_id"] {
            if let id = a[k] as? String, let i = (0..<m.partCount).first(where: { m.partId(forPart: $0) == id }) {
                if let p = focus.point, focus.part == i { return .point(p) }
                return .part(i)
            }
        }
        if let p = focus.point { return .point(p) }
        return focus.part.map { .part($0) }
    }
}

extension AgentCore {
    /// Extensions `EditorModel.openDocument` handles as documents/imports.
    nonisolated static let openableExtensions: Set<String> = ["vcad", "loon", "dxf", "step", "stp", "urdf"]

    /// `path` as a URL when it names an existing openable document (or a
    /// folder, which opens its loon entry program); nil otherwise.
    nonisolated static func openableDocument(_ path: String) -> URL? {
        let expanded = (path as NSString).expandingTildeInPath
        guard expanded.hasPrefix("/") else { return nil }
        let url = URL(fileURLWithPath: expanded).standardizedFileURL
        var isDir: ObjCBool = false
        guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir) else { return nil }
        if isDir.boolValue { return url }
        return openableExtensions.contains(url.pathExtension.lowercased()) ? url : nil
    }
}
