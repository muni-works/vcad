#if canImport(AppKit)
import AppKit
#endif
import Foundation
import Darwin
import Network
import RealityKit
import simd
import SwiftUI

// Agent bridge: the app serves MCP on localhost so a Claude session and the
// user share ONE view — the release-to-desktop viewport. The agent reads what
// the user is pointing at (selection / hover), drives pose, explode and
// camera, lights parts and pins labels the user sees, and takes snapshots of
// exactly what is on screen.
//
// The viewport tools here are read/drive only. Document edits live in
// AgentEdits.swift: each is one named undo step, applied live. A generated
// `.loon` stays owned by its generator (view-only); regenerates arrive
// through the file watch.
//
// Transport: MCP streamable HTTP, JSON responses only (no SSE), at
// http://127.0.0.1:<port>/mcp. The port and pid land in ~/.vcad/instances/<pid>.json
// (+ the ~/.vcad/app.json "most recent" pointer) for the stdio shim (apple/VcadApp/scripts/vcad-app-mcp.mjs). Loopback only.

/// A label an agent pinned to an instance.
struct AgentAnnotation: Identifiable, Equatable {
    let id: String
    let instance: Int
    var text: String
}

@MainActor
final class AgentBridge {
    static let shared = AgentBridge()
    static let basePort: UInt16 = 47_361

    private weak var model: EditorModel?
    private var listener: NWListener?
    private(set) var port: UInt16?

    func start(model: EditorModel) {
        self.model = model
        AgentResources.shared.store = AppStore(model: model)
        guard listener == nil else { return }
        guard ProcessInfo.processInfo.environment["VCAD_AGENT_BRIDGE"] != "0" else { return }
        // VCAD_AGENT_PORT pins the port (a second instance during development
        // would otherwise share 47361 through endpoint reuse).
        let pinned = ProcessInfo.processInfo.environment["VCAD_AGENT_PORT"].flatMap { UInt16($0) }
        listen(on: pinned ?? Self.basePort, attemptsLeft: pinned == nil ? 8 : 0)
    }

    private func listen(on p: UInt16, attemptsLeft: Int) {
        let params = NWParameters.tcp
        params.requiredInterfaceType = .loopback
        params.allowLocalEndpointReuse = true
        guard let port = NWEndpoint.Port(rawValue: p),
              let l = try? NWListener(using: params, on: port) else {
            if attemptsLeft > 0 { listen(on: p + 1, attemptsLeft: attemptsLeft - 1) }
            return
        }
        l.newConnectionHandler = { conn in
            MainActor.assumeIsolated { AgentBridge.shared.accept(conn) }
        }
        l.stateUpdateHandler = { state in
            MainActor.assumeIsolated {
                switch state {
                case .ready:
                    AgentBridge.shared.port = p
                    AgentBridge.shared.writeDiscovery(port: p)
                case .failed:
                    AgentBridge.shared.listener?.cancel()
                    AgentBridge.shared.listener = nil
                    if attemptsLeft > 0 { AgentBridge.shared.listen(on: p + 1, attemptsLeft: attemptsLeft - 1) }
                default: break
                }
            }
        }
        listener = l
        l.start(queue: .main)
    }

    /// Random per launch; agents compare it across calls to notice a relaunch.
    let instanceId = UUID().uuidString.lowercased()
    private let launched: String = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f.string(from: Date())
    }()
    private var lastRecord: AgentInstanceRecord?
    private var discoveryTimer: Timer?

    /// This instance as clients see it (discovery file + every response's `_meta`).
    var record: AgentInstanceRecord? {
        guard let port else { return nil }
        return AgentInstanceRecord(pid: Int(ProcessInfo.processInfo.processIdentifier), port: Int(port),
                                   url: "http://127.0.0.1:\(port)/mcp", instanceId: instanceId, started: launched,
                                   documentName: model?.documentName, documentPath: model?.documentURL?.path)
    }

    /// ~/.vcad/instances/<pid>.json (+ the app.json "most recent" pointer) —
    /// how the stdio shim and @vcad/mcp find us. See AgentDiscovery.swift.
    private func writeDiscovery(port: UInt16) {
        refreshDiscovery()
        // The open document changes under us; keep the file current so clients
        // can pick the window by document path.
        discoveryTimer?.invalidate()
        discoveryTimer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { _ in
            MainActor.assumeIsolated { AgentBridge.shared.refreshDiscovery() }
        }
        #if canImport(AppKit)
        NotificationCenter.default.addObserver(forName: NSApplication.willTerminateNotification, object: nil,
                                               queue: .main) { _ in
            AgentDiscovery.remove(pid: Int(ProcessInfo.processInfo.processIdentifier))
        }
        #endif
        FileHandle.standardError.write(Data("[vcad] agent bridge on http://127.0.0.1:\(port)/mcp\n".utf8))
    }

    func refreshDiscovery() {
        guard let r = record, r != lastRecord else { return }
        lastRecord = r
        AgentDiscovery.write(r)
    }

    /// Names this instance on every response, so an agent can tell which window it is driving.
    var instanceMeta: [String: Any] {
        guard let r = record else { return [:] }
        return ["pid": r.pid, "port": r.port, "instance_id": r.instanceId, "started": r.started,
                "document": r.documentName ?? NSNull(), "document_path": r.documentPath ?? NSNull()]
    }

    // MARK: HTTP (just enough: one request per connection, Content-Length bodies)

    private func accept(_ conn: NWConnection) {
        conn.start(queue: .main)
        receive(conn, buffer: Data())
    }

    private func receive(_ conn: NWConnection, buffer: Data) {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 1 << 20) { data, _, done, error in
            MainActor.assumeIsolated {
                var buf = buffer
                if let data { buf.append(data) }
                if let req = HTTPRequest.parse(buf) {
                    AgentBridge.shared.handle(req, conn)
                } else if done || error != nil || buf.count > 32 << 20 {
                    conn.cancel()
                } else {
                    AgentBridge.shared.receive(conn, buffer: buf)
                }
            }
        }
    }

    private func respond(_ conn: NWConnection, status: String, body: Data = Data(),
                         contentType: String = "application/json") {
        var head = "HTTP/1.1 \(status)\r\nContent-Type: \(contentType)\r\nContent-Length: \(body.count)\r\nConnection: close\r\n"
        head += "Mcp-Session-Id: vcad-app\r\n"
        if let r = record { head += "X-Vcad-Instance: \(r.pid) \(r.instanceId)\r\n" }
        head += "\r\n"
        var out = Data(head.utf8)
        out.append(body)
        conn.send(content: out, completion: .contentProcessed { _ in conn.cancel() })
    }

    private func handle(_ req: HTTPRequest, _ conn: NWConnection) {
        // A browser page must not drive the app: refuse any request carrying a
        // non-loopback Origin (DNS-rebinding guard, per the MCP transport spec).
        if let origin = req.headers["origin"], !origin.contains("127.0.0.1"), !origin.contains("localhost") {
            respond(conn, status: "403 Forbidden"); return
        }
        guard req.path.hasPrefix("/mcp") else { respond(conn, status: "404 Not Found"); return }
        // Server→client stream: resource-update notifications (AgentResources).
        if req.method == "GET", req.headers["accept"]?.contains("text/event-stream") == true {
            AgentResources.shared.attachStream(conn); return
        }
        guard req.method == "POST" else { respond(conn, status: "405 Method Not Allowed"); return }
        guard let msg = (try? JSONSerialization.jsonObject(with: req.body)) as? [String: Any] else {
            respond(conn, status: "400 Bad Request", body: Self.rpcError(nil, -32700, "parse error")); return
        }
        guard let id = msg["id"] else { respond(conn, status: "202 Accepted"); return }   // notification
        let method = msg["method"] as? String ?? ""
        let params = msg["params"] as? [String: Any] ?? [:]
        Task { @MainActor in
            let body: Data
            switch method {
            case "initialize":
                if let client = (params["clientInfo"] as? [String: Any])?["name"] as? String {
                    AgentCursor.shared.agentName = Self.displayName(client)
                }
                body = Self.rpcResult(id, [
                    "protocolVersion": (params["protocolVersion"] as? String) ?? "2025-06-18",
                    "capabilities": ["tools": ["listChanged": false],
                                     "resources": ["subscribe": true, "listChanged": false]],
                    "serverInfo": ["name": "vcad-app", "version": "0.2"],
                    "instructions": Self.instructions,
                    "_meta": ["io.vcad/instance": self.instanceMeta],
                ])
            case "ping":
                body = Self.rpcResult(id, [:])
            case "tools/list":
                body = Self.rpcResult(id, ["tools": AgentTools.definitions + Self.withViewer(AgentEdits.definitions)
                                                       + AnalysisAgentTools.definitions
                                                       + ProofAgentTools.definitions
                                                       + [AgentTurn.toolDefinition] + AgentViewer.definitions])
            case "resources/list":
                body = Self.rpcResult(id, AgentResources.shared.list())
            case "resources/read", "resources/subscribe", "resources/unsubscribe":
                let uri = params["uri"] as? String ?? ""
                do {
                    switch method {
                    case "resources/read": body = Self.rpcResult(id, try AgentResources.shared.read(uri))
                    case "resources/subscribe":
                        try AgentResources.shared.subscribe(uri); body = Self.rpcResult(id, [:])
                    default:
                        AgentResources.shared.unsubscribe(uri); body = Self.rpcResult(id, [:])
                    }
                } catch let e as AgentTools.Failure {
                    body = Self.rpcError(id, -32002, e.message)
                } catch {
                    body = Self.rpcError(id, -32603, "\(error)")
                }
            case "tools/call":
                let name = params["name"] as? String ?? ""
                let args = params["arguments"] as? [String: Any] ?? [:]
                // MRTR retry payload: elicitations the client already answered.
                self.refreshDiscovery()
                let result = Self.stampInstance(await self.callTool(name, args, inputResponses: params["inputResponses"]),
                                                self.instanceMeta)
                body = Self.rpcResult(id, result)
            default:
                body = Self.rpcError(id, -32601, "method not found: \(method)")
            }
            self.respond(conn, status: "200 OK", body: body)
        }
    }

    /// Adds `_meta["io.vcad/instance"]` and a trailing one-line text block
    /// ("vcad.app pid 123 · bracket.vcad") to a tool result. Trailing, so
    /// clients that parse content[0] as JSON are unaffected.
    static func stampInstance(_ result: [String: Any], _ meta: [String: Any]) -> [String: Any] {
        guard let pid = meta["pid"] else { return result }
        var r = result
        var m = r["_meta"] as? [String: Any] ?? [:]
        m["io.vcad/instance"] = meta
        r["_meta"] = m
        let doc = (meta["document"] as? String) ?? "no document"
        var content = r["content"] as? [[String: Any]] ?? []
        content.append(["type": "text", "text": "[vcad.app pid \(pid) · \(doc)]"])
        r["content"] = content
        return r
    }

    /// "claude-code" → "Claude", "cursor-vscode" → "Cursor".
    static func displayName(_ client: String) -> String {
        let first = client.split(whereSeparator: { $0 == "-" || $0 == " " || $0 == "_" }).first.map(String.init) ?? client
        return first.isEmpty ? "Agent" : first.prefix(1).uppercased() + first.dropFirst()
    }

    /// Mutating tools render in the live viewer (MCP Apps `_meta.ui`).
    static func withViewer(_ tools: [[String: Any]]) -> [[String: Any]] {
        tools.map { t in
            guard let n = t["name"] as? String, !AgentEdits.readEngineTools.contains(n),
                  !["get_document", "inspect_cad", "list_parameters", "link_status", "attach_document"].contains(n)
            else { return t }
            var t = t; t["_meta"] = AgentViewer.meta; return t
        }
    }

    static let instructions = """
    You are sharing the user's live vcad desktop viewport. Call app_state first: it says what is \
    open, the joint pose, explode, camera, and what the user has selected or is hovering — when \
    the user says "this part", that is it. Everything you change (pose, explode, camera, \
    highlights, labels) is visible to the user immediately. Use snapshot to see the view. \
    You can also author the open document: create / update / delete / set_material, booleans \
    (union, difference, intersection), transforms, patterns, fillet/chamfer/shell via create, \
    set_parameters, and create_cad_loon for a whole program. Build in small steps — each call \
    is one named undo step the user watches land and can ⌘Z one at a time. Read with \
    describe_scene, inspect_cad, list_parameters and get_document. Units are mm, Z is up. \
    Generated .loon documents and electronics are view-only. \
    Resources: vcad://doc/selection is what the user picked (subscribe to follow it; "this edge" means \
    it), vcad://doc/current the live document, vcad://doc/view the camera. Before a multi-step change \
    call agent_turn {label:"thickened walls"} so it lands as ONE undo step, and agent_turn {end:true} \
    after. fillet_edges fillets the user's selected edges.
    """

    static func rpcResult(_ id: Any, _ result: [String: Any]) -> Data {
        (try? JSONSerialization.data(withJSONObject: ["jsonrpc": "2.0", "id": id, "result": result])) ?? Data()
    }
    static func rpcError(_ id: Any?, _ code: Int, _ message: String) -> Data {
        (try? JSONSerialization.data(withJSONObject: ["jsonrpc": "2.0", "id": id ?? NSNull(),
                                                      "error": ["code": code, "message": message]])) ?? Data()
    }

    // MARK: tools

    private func callTool(_ name: String, _ args: [String: Any], inputResponses: Any? = nil) async -> [String: Any] {
        guard let model else { return AgentTools.error("The app has no window yet.") }
        do {
            // Proof tools (receipts, viewport elicitation) — ProofAgentTools.swift.
            if ProofAgentTools.toolNames.contains(name) {
                return try await ProofAgentTools.call(name, args, inputResponses: inputResponses, model: model)
            }
            if let refused = await ProofAgentTools.confirmDestructive(name, args, inputResponses: inputResponses,
                                                                      model: model) {
                return refused
            }
            return try await AgentTools.call(name, args, model: model)
        } catch let e as AgentTools.Failure {
            return AgentTools.error(e.message)
        } catch {
            return AgentTools.error("\(error)")
        }
    }
}

/// One parsed HTTP request (nil until the whole body has arrived).
struct HTTPRequest {
    let method: String
    let path: String
    let headers: [String: String]
    let body: Data

    static func parse(_ buf: Data) -> HTTPRequest? {
        guard let sep = buf.range(of: Data("\r\n\r\n".utf8)) else { return nil }
        let head = String(decoding: buf[..<sep.lowerBound], as: UTF8.self)
        var lines = head.components(separatedBy: "\r\n")
        let start = lines.removeFirst().split(separator: " ")
        guard start.count >= 2 else { return nil }
        var headers: [String: String] = [:]
        for l in lines {
            guard let c = l.firstIndex(of: ":") else { continue }
            headers[l[..<c].lowercased()] = l[l.index(after: c)...].trimmingCharacters(in: .whitespaces)
        }
        let len = Int(headers["content-length"] ?? "0") ?? 0
        let bodyStart = sep.upperBound
        guard buf.count - bodyStart >= len else { return nil }
        return HTTPRequest(method: String(start[0]), path: String(start[1]), headers: headers,
                           body: buf[bodyStart..<(bodyStart + len)])
    }
}

@MainActor
enum AgentTools {
    struct Failure: Error { let message: String }

    static func text(_ s: String) -> [String: Any] { ["content": [["type": "text", "text": s]]] }
    static func error(_ s: String) -> [String: Any] { ["content": [["type": "text", "text": s]], "isError": true] }
    static func json(_ obj: Any) -> [String: Any] {
        let data = (try? JSONSerialization.data(withJSONObject: obj, options: [.prettyPrinted, .sortedKeys])) ?? Data()
        return text(String(decoding: data, as: UTF8.self))
    }

    private static func schema(_ props: [String: Any], required: [String] = []) -> [String: Any] {
        ["type": "object", "properties": props, "required": required]
    }
    private static let partsProp: [String: Any] = [
        "type": "array", "items": ["type": "string"],
        "description": "Instance ids or name fragments (e.g. \"cover-knee_ll\", \"knee_ll\"), or a joint name (\"l_knee_pitch\") for the link it drives.",
    ]

    static let definitions: [[String: Any]] = [
        ["name": "app_state",
         "description": "What the user is looking at: open document, every driveable joint (value + limits), explode, camera, the user's selection and hover, and your current highlights/labels. Call this first, and again when the user says \"this\".",
         "inputSchema": schema([:])],
        ["name": "list_parts",
         "description": "Part ids in the open document: assembly instances when it is an assembly, otherwise the document's parts (usable as part_id / child in edit tools).",
         "inputSchema": schema([:])],
        ["name": "open_document",
         "description": "Open a .vcad or .loon file in the shared view (read-only; .loon files hot-reload when regenerated).",
         "inputSchema": schema(["path": ["type": "string"]], required: ["path"])],
        ["name": "set_pose",
         "description": "Set joint angles in degrees (clamped to limits), by joint name, e.g. {\"l_knee_pitch\": 60}. reset:true first returns every joint to its authored value.",
         "inputSchema": schema(["joints": ["type": "object", "additionalProperties": ["type": "number"]],
                                "reset": ["type": "boolean"],
                                "base": ["type": "array", "items": ["type": "number"],
                                         "description": "Floating base pose: x, y, z in metres, then w, x, y, z"],
                                "restart": ["type": "boolean", "description": "base starts a new episode"],
                                "link_id": ["type": "string", "description": "Sent by @vcad/mcp: the session this pose belongs to."],
                                "partial": ["type": "boolean", "description": "Sent by @vcad/mcp for a mirrored stream: skip joints this window doesn't drive."]])],
        ["name": "set_explode",
         "description": "Explode amount 0 (assembled) … 1 (fully exploded).",
         "inputSchema": schema(["amount": ["type": "number"]], required: ["amount"])],
        ["name": "set_camera",
         "description": "Orbit the shared camera. azimuth/elevation in degrees (Z-up; azimuth 0 looks from the front), distance in view units (1.5 ≈ whole model).",
         "inputSchema": schema(["azimuth": ["type": "number"], "elevation": ["type": "number"],
                                "distance": ["type": "number"]])],
        ["name": "frame_part",
         "description": "Point the camera at a part and pull in to fit it.",
         "inputSchema": schema(["part": ["type": "string"]], required: ["part"])],
        ["name": "highlight_parts",
         "description": "Light parts in brand orange so the user sees what you mean. Replaces the previous highlight unless add:true; parts:[] clears.",
         "inputSchema": schema(["parts": partsProp, "add": ["type": "boolean"]], required: ["parts"])],
        ["name": "annotate",
         "description": "Pin a text label to a part, drawn in the user's viewport with a leader to the part. Same part again replaces its label.",
         "inputSchema": schema(["part": ["type": "string"], "text": ["type": "string"]], required: ["part", "text"])],
        ["name": "clear_annotations",
         "description": "Remove your labels (and highlights, with highlights:true).",
         "inputSchema": schema(["highlights": ["type": "boolean"]])],
        ["name": "snapshot",
         "description": "PNG of the shared viewport exactly as the user sees it (geometry, highlights; labels are an overlay and not included).",
         "inputSchema": schema([:])],
        ["name": "app_screenshot",
         "description": "PNG of the whole editor window as the user sees it: toolbar, panels, dock, edit bar, suggestion pill.",
         "inputSchema": schema([:])],
        ["name": "set_workspace",
         "description": "Switch the dock workspace: design, assembly, drawing, electronics or fab. history:true/false opens or closes the history filmstrip.",
         "inputSchema": schema(["workspace": ["type": "string"], "history": ["type": "boolean"]], required: ["workspace"])],
        ["name": "select_feature",
         "description": "Select a feature-tree node by name (case-insensitive substring) — the user sees it selected, and its edit bar opens beside it.",
         "inputSchema": schema(["name": ["type": "string"]], required: ["name"])],
        ["name": "history_seek",
         "description": "Move the history playhead: frame 0 is the document as opened, frame k is after the k-th undo step. Rewinds/replays and rebuilds the viewport; returns the rebuild time.",
         "inputSchema": schema(["frame": ["type": "integer"]], required: ["frame"])],
        ["name": "import_board",
         "description": "Import a KiCad (.kicad_pcb) or Eagle (.brd) board file into the open document and switch to Electronics.",
         "inputSchema": schema(["path": ["type": "string"]], required: ["path"])],
        ["name": "press_key",
         "description": "Press a bare-letter tool shortcut as if typed in the viewport (e.g. \"f\" for Fillet, \"?\" for the shortcut sheet). Returns whether a tool was bound.",
         "inputSchema": schema(["key": ["type": "string"]], required: ["key"])],
        ["name": "suggest_edit",
         "description": "Offer ONE edit as a suggestion pill in the user's viewport instead of applying it: a short question (\"Add 4 M3 counterbores on the motor bolt circle?\") plus the edit tool call it would make. The parts it touches shimmer softly. Nothing changes until the user clicks Add (then it is one named undo step); × dismisses. A new suggestion replaces the old one.",
         "inputSchema": schema(["text": ["type": "string", "description": "One-line question shown on the pill."],
                                "tool": ["type": "string", "description": "An edit tool name (create, update, difference, set_parameters, …)."],
                                "args": ["type": "object", "description": "That tool's arguments."],
                                "parts": partsProp],
                               required: ["text", "tool", "args"])],
        ["name": "photoreal",
         "description": "Path-trace the current pose/explode from the current camera (the vcad render --photoreal renderer, ~30-60 s). Returns the image and its saved path.",
         "inputSchema": schema(["spp": ["type": "integer", "description": "samples per pixel, default 64"]])],
    ]

    private static let setPoseArgs: Set<String> = ["joints", "reset", "base", "restart", "link_id", "partial"]

    /// Validates everything before touching the pose, so a malformed call
    /// changes nothing. `partial` is for a mirrored stream, which carries joints
    /// this window doesn't drive: it poses what matches instead of refusing.
    static func setPose(_ a: [String: Any], _ m: EditorModel) throws -> [String: Any] {
        let usage = "Expected {\"joints\": {\"<joint name>\": degrees}}, optionally with reset or base."
        let stray = a.keys.filter { !setPoseArgs.contains($0) }.sorted()
        guard stray.isEmpty else { throw Failure(message: "set_pose: unknown argument \(stray.joined(separator: ", ")). \(usage)") }
        guard m.hasPoseStudio || m.poseAwaitsScene else { throw Failure(message: "The open document has no poseable assembly.") }
        if let link = a["link_id"] as? String, m.agentLinkID != link {
            throw Failure(message: "set_pose: this window is showing a different session.")
        }
        let partial = a["partial"] as? Bool == true
        let requested: [String: Any]
        switch a["joints"] {
        case nil: requested = [:]
        case let d as [String: Any]: requested = d
        default: throw Failure(message: "set_pose: joints must be an object. \(usage)")
        }
        let names = m.drivenJoints.map(EditorModel.poseKey).joined(separator: ", ")
        var unknown: [String] = [], targets: [(key: String, degrees: Double)] = []
        for (k, v) in requested {
            guard let deg = (v as? NSNumber)?.doubleValue, deg.isFinite else {
                throw Failure(message: "set_pose: joint \(k) needs a number of degrees. \(usage)")
            }
            if let j = m.drivenJoints.first(where: { EditorModel.poseKey($0) == k || $0.id == k }) {
                let clamped = j.limits.map { min(max(deg, $0.lowerBound), $0.upperBound) } ?? deg
                targets.append((EditorModel.poseKey(j), clamped))
            } else if m.documentGraph?.joints.contains(where: { EditorModel.poseKey($0) == k || $0.id == k }) != true {
                unknown.append(k)
            }
        }
        unknown.sort()
        if !unknown.isEmpty && !partial {
            throw Failure(message: "set_pose: unknown joints \(unknown.joined(separator: ", ")). Names are \(names).")
        }
        var base: [Double]?
        if let raw = a["base"] {
            if let b = (raw as? [NSNumber])?.map(\.doubleValue), b.count == 7 { base = b }
            else if !partial { throw Failure(message: "set_pose: base must be 7 numbers: x, y, z in metres, then w, x, y, z.") }
        }
        let reset = a["reset"] as? Bool == true
        guard !targets.isEmpty || reset || base != nil else {
            throw Failure(message: "set_pose: nothing to apply. \(usage) Names are \(names).")
        }
        if let link = a["link_id"] as? String, m.agentLinkID == link { m.agentLinkActiveAt = Date() }
        AgentEdits.surfacePose(m)
        if reset { m.jointPose = [:] }
        for t in targets { m.jointPose[t.key] = t.degrees }
        m.clearJointDrags(forPoseKeys: Set(targets.map(\.key)))
        if let base { m.setAgentBasePose(base, restart: a["restart"] as? Bool == true) }
        m.applyStudioPose()
        var msg = "\(m.hasPoseStudio ? "Pose applied" : "Pose held until the scene is ready"): \(targets.map { "\($0.key)=\(String(format: "%.1f", $0.degrees))°" }.sorted().joined(separator: ", "))"
        if !unknown.isEmpty { msg += ". Unknown joints: \(unknown.joined(separator: ", ")) — names are \(names)" }
        return text(msg)
    }

    static func call(_ name: String, _ a: [String: Any], model m: EditorModel) async throws -> [String: Any] {
        switch name {
        case "app_state": return json(state(m))
        case "list_parts":
            // An assembly lists its instances; a part document (no instances)
            // lists the engine's parts, so the ids work in inspect/translate/…
            // instead of coming back empty.
            if !m.assemblyInstanceIDs.isEmpty {
                return json(m.assemblyInstanceIDs.enumerated().map { i, id in
                    ["id": id, "index": i, "kind": "instance", "highlighted": m.agentHighlights.contains(i)] as [String: Any]
                })
            }
            return json((m.engineState?.parts ?? []).enumerated().map { i, p in
                ["id": p.id, "index": i, "kind": "part"] as [String: Any]
            })
        case "open_document":
            guard let path = a["path"] as? String else { throw Failure(message: "path is required") }
            let url = URL(fileURLWithPath: (path as NSString).expandingTildeInPath)
            guard FileManager.default.fileExists(atPath: url.path) else { throw Failure(message: "no file at \(url.path)") }
            m.openDocument(url)
            return text("Opened \(url.lastPathComponent). It solves in the background; call app_state to see when instances appear.")
        case "set_pose":
            return try setPose(a, m)
        case "set_explode":
            guard let t = (a["amount"] as? NSNumber)?.doubleValue else { throw Failure(message: "amount is required") }
            m.setExplode(t)
            return text("Explode \(Int(m.explode * 100))%")
        case "set_camera":
            m.stopSpin()
            withAnimation(.smooth(duration: 0.4)) {
                if let az = (a["azimuth"] as? NSNumber)?.floatValue { m.azimuth = az * .pi / 180 }
                if let el = (a["elevation"] as? NSNumber)?.floatValue { m.elevation = max(-1.5, min(1.5, el * .pi / 180)) }
                if let d = (a["distance"] as? NSNumber)?.floatValue { m.distance = max(0.2, min(8, d)) }
            }
            m.pinchBaseline = m.distance
            return json(camera(m))
        case "frame_part":
            let idx = try resolve([a["part"] as? String ?? ""], m)
            guard let i = idx.first else { throw Failure(message: "no part matched") }
            ReleaseWindowController.shared.frame(entityNamed: "inst\(i)")
            return text("Framed \(m.assemblyInstanceIDs[i]).")
        case "highlight_parts":
            let names = (a["parts"] as? [String]) ?? []
            let idx = names.isEmpty ? [] : try resolve(names, m)
            m.agentHighlights = a["add"] as? Bool == true ? m.agentHighlights.union(idx) : Set(idx)
            ReleaseWindowController.shared.repaintAll()
            return text(idx.isEmpty ? "Highlights cleared."
                        : "Highlighted \(idx.count): \(idx.map { m.assemblyInstanceIDs[$0] }.joined(separator: ", "))")
        case "annotate":
            let part = a["part"] as? String ?? ""
            let note = a["text"] as? String ?? ""
            guard let i = try resolve([part], m).first else { throw Failure(message: "no part matched") }
            let id = m.assemblyInstanceIDs[i]
            m.agentAnnotations.removeAll { $0.instance == i }
            if !note.isEmpty { m.agentAnnotations.append(AgentAnnotation(id: id, instance: i, text: note)) }
            return text("Label on \(id): \(note)")
        case "clear_annotations":
            m.agentAnnotations = []
            if a["highlights"] as? Bool == true {
                m.agentHighlights = []
                ReleaseWindowController.shared.repaintAll()
            }
            return text("Cleared.")
        case "suggest_edit":
            let tool = a["tool"] as? String ?? ""
            let reads: Set<String> = ["get_document", "inspect_cad", "list_parameters", "undo"]
            guard AgentEdits.toolNames.contains(tool), !reads.contains(tool), !AgentEdits.readEngineTools.contains(tool) else {
                throw Failure(message: "suggest_edit needs an edit tool; \(tool) is not one.")
            }
            let args = (a["args"] as? [String: Any] ?? [:]).compactMapValues { $0 as? AnyHashable }
            let names = (a["parts"] as? [String]) ?? []
            // Engine part ids → viewport part indices (visible roots).
            let visible = (0..<max(m.partCount, 1)).compactMap { pi in m.partId(forPart: pi).map { (pi, $0) } }
            let byId = names.compactMap { n in visible.first { $0.1 == n }?.0 }
            let parts = !byId.isEmpty ? byId : (names.isEmpty ? [] : ((try? resolve(names, m)) ?? []))
            GlassUI.shared.suggestion = AgentSuggestion(text: a["text"] as? String ?? "Apply this edit?",
                                                        tool: tool, args: args, parts: parts)
            m.agentShimmer = Set(parts)
            GlassUI.shared.startShimmer(model: m)
            return text("Suggestion shown. The user decides; app_state.suggestion reports whether it is still pending.")
        case "history_seek":
            let total = m.undoStack.count + m.redoStack.count
            let f = max(0, min(total, (a["frame"] as? NSNumber)?.intValue ?? total))
            let ms = await GlassUI.seek(model: m, to: f)
            return text(String(format: "At frame %d of %d; rebuilt in %.0f ms.", f, total, ms))
        case "import_board":
            let path = (a["path"] as? String ?? "") as NSString
            guard FileManager.default.fileExists(atPath: path.expandingTildeInPath) else { throw Failure(message: "no such file") }
            GlassUI.shared.enter(.electronics, model: m)
            m.electronics.tools.importBoard(URL(fileURLWithPath: path.expandingTildeInPath), m.electronics, m)
            return text("Importing \(path.lastPathComponent)…")
        case "press_key":
            guard let ch = (a["key"] as? String)?.first else { throw Failure(message: "key required") }
            let ok = GlassUI.shared.handleKey(ch, model: m)
            return text(ok ? "Handled \(ch)." : "Nothing bound to \(ch) here.")
        case "set_workspace":
            guard let ws = DockSpace(rawValue: (a["workspace"] as? String ?? "").lowercased()) else {
                throw Failure(message: "workspace must be one of design, assembly, drawing, electronics, fab")
            }
            GlassUI.shared.enter(ws, model: m)
            if let h = a["history"] as? Bool { GlassUI.shared.historyOpen = h }
            return text("Workspace: \(ws.label).")
        case "select_feature":
            let q = a["name"] as? String ?? ""
            func find(_ ns: [FeatureNode]) -> FeatureNode? {
                for n in ns {
                    if n.name.localizedCaseInsensitiveContains(q) || n.opType.localizedCaseInsensitiveContains(q) { return n }
                    if let c = find(n.children) { return c }
                }
                return nil
            }
            guard let n = find(m.featureNodes) else { throw Failure(message: "no feature matched \(q)") }
            m.selectFeature(n.id)
            return text("Selected \(n.name) (\(n.opType)).")
        case "app_screenshot":
            guard let png = await windowPNG() else { throw Failure(message: "No editor window.") }
            return ["content": [["type": "image", "data": png.base64EncodedString(), "mimeType": "image/png"]]]
        case "snapshot":
            guard let png = await snapshotPNG() else { throw Failure(message: "The viewport is not on screen.") }
            return ["content": [["type": "image", "data": png.base64EncodedString(), "mimeType": "image/png"],
                                ["type": "text", "text": "Viewport \(m.documentName); labels: \(m.agentAnnotations.map { "\($0.id): \($0.text)" })"]]]
        case "photoreal":
            let spp = (a["spp"] as? NSNumber)?.intValue ?? 64
            let (path, err): (String?, String?) = await withCheckedContinuation { cont in
                m.photorealSnapshot(spp: spp) { p, e in cont.resume(returning: (p, e)) }
            }
            guard let path else { throw Failure(message: err ?? "photoreal failed") }
            var content: [[String: Any]] = [["type": "text", "text": "Saved \(path)"]]
            if let img = NSImage(contentsOfFile: path), let png = downscaledPNG(img, maxSide: 1000) {
                content.insert(["type": "image", "data": png.base64EncodedString(), "mimeType": "image/png"], at: 0)
            }
            return ["content": content]
        case "agent_turn":
            return AgentTurn.shared.call(a)
        case _ where AgentViewer.toolNames.contains(name):
            return try await AgentViewer.call(name, a, model: m)
        case _ where AgentEdits.toolNames.contains(name):
            return try AgentEdits.call(name, a, model: m)
        case _ where AnalysisAgentTools.toolNames.contains(name):
            return try await AnalysisAgentTools.call(name, a, model: m)
        default:
            throw Failure(message: "unknown tool \(name)")
        }
    }

    // MARK: state

    static func state(_ m: EditorModel) -> [String: Any] {
        let ids = m.assemblyInstanceIDs
        func name(_ i: Int?) -> Any { i.flatMap { $0 < ids.count ? ids[$0] : nil } ?? NSNull() }
        var s: [String: Any] = [
            "document": ["name": m.documentName, "path": m.documentURL?.path ?? NSNull(),
                         "readOnlyGenerated": m.isLoonDocument,
                         "solving": m.solving, "instances": m.assemblyInstanceCount] as [String: Any],
            "camera": camera(m),
            "explode": m.explode,
            "user": ["selected": m.selectedInstances.sorted().map { ids.indices.contains($0) ? ids[$0] : "\($0)" },
                     "hovered": name(m.hoveredInstanceIndex)] as [String: Any],
            "agent": ["highlights": m.agentHighlights.sorted().map { ids.indices.contains($0) ? ids[$0] : "\($0)" },
                      "labels": m.agentAnnotations.map { ["part": $0.id, "text": $0.text] }] as [String: Any],
            "mode": m.isWindowed ? "windowed" : "released-to-desktop",
            "suggestion": GlassUI.shared.suggestion?.text ?? NSNull(),
            "workspace_dock": GlassUI.shared.space.rawValue,
        ]
        if let t = m.lastReload { s["lastReload"] = ISO8601DateFormatter().string(from: t) }
        s["joints"] = m.drivenJoints.map { j -> [String: Any] in
            var o: [String: Any] = ["name": EditorModel.poseKey(j), "value": m.jointValue(j), "kind": j.kind,
                                    "child": j.childInstanceId]
            if let l = j.limits { o["min"] = l.lowerBound; o["max"] = l.upperBound }
            return o
        }
        return s
    }

    static func camera(_ m: EditorModel) -> [String: Any] {
        ["azimuth": Double(m.azimuth) * 180 / .pi, "elevation": Double(m.elevation) * 180 / .pi,
         "distance": Double(m.distance)]
    }

    /// Names → instance indices. Exact id, id without the `_i` suffix, a joint
    /// name (its child link), then substring. Throws listing near misses.
    static func resolve(_ names: [String], _ m: EditorModel) throws -> [Int] {
        let ids = m.assemblyInstanceIDs
        guard !ids.isEmpty else { throw Failure(message: "No assembly is open.") }
        var out: [Int] = []
        for raw in names {
            let n = raw.trimmingCharacters(in: .whitespaces)
            if let i = ids.firstIndex(where: { $0 == n || $0 == n + "_i" }) { out.append(i); continue }
            if let j = m.drivenJoints.first(where: { EditorModel.poseKey($0) == n }),
               let i = ids.firstIndex(of: j.childInstanceId) { out.append(i); continue }
            let hits = ids.indices.filter { ids[$0].localizedCaseInsensitiveContains(n) }
            guard !hits.isEmpty else {
                throw Failure(message: "No part matches \"\(n)\". Call list_parts for ids.")
            }
            out += hits
        }
        return Array(Set(out)).sorted()
    }

    // MARK: images

    /// The whole editor window — chrome included — as the user sees it.
    /// Uses the window server's own-window capture (no Screen Recording grant
    /// is needed for a process's own windows); falls back to compositing the
    /// viewport snapshot under a cached render of the SwiftUI chrome.
    static func windowPNG() async -> Data? {
        guard let w = ReleaseWindowController.shared.hostWindow else { return nil }
        typealias Capture = @convention(c) (CGRect, UInt32, UInt32, UInt32) -> Unmanaged<CGImage>?
        if let sym = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "CGWindowListCreateImage") {
            let fn = unsafeBitCast(sym, to: Capture.self)
            // .optionIncludingWindow = 1 << 3; .boundsIgnoreFraming = 1 << 0
            if let img = fn(.null, 1 << 3, UInt32(w.windowNumber), 1 << 0)?.takeRetainedValue(),
               img.width > 1 {
                return downscaledPNG(NSImage(cgImage: img, size: .zero), maxSide: 1600)
            }
        }
        guard let view = w.contentView else { return nil }
        let size = view.bounds.size
        let out = NSImage(size: size)
        out.lockFocus()
        if let vp = await snapshotPNG(), let vimg = NSImage(data: vp), let ar = ReleaseWindowController.shared.arView {
            let r = ar.convert(ar.bounds, to: nil)
            vimg.draw(in: r)
        }
        if let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) {
            view.cacheDisplay(in: view.bounds, to: rep)
            NSImage(size: size, flipped: false) { rect in rep.draw(in: rect) }.draw(in: view.bounds)
        }
        out.unlockFocus()
        return downscaledPNG(out, maxSide: 1600)
    }

    static func snapshotPNG() async -> Data? {
        guard let ar = ReleaseWindowController.shared.arView else { return nil }
        let cg: CGImage? = await withCheckedContinuation { cont in
            RenderGate.wake(for: 2)  // a paused view never delivers a snapshot
            ar.snapshot(saveToHDR: false) { image in
                cont.resume(returning: image?.cgImage(forProposedRect: nil, context: nil, hints: nil))
            }
        }
        guard let cg else { return nil }
        return downscaledPNG(NSImage(cgImage: cg, size: .zero), maxSide: 1400)
    }

    static func downscaledPNG(_ img: NSImage, maxSide: CGFloat) -> Data? {
        guard let cg = img.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return nil }
        let w = CGFloat(cg.width), h = CGFloat(cg.height)
        let s = min(1, maxSide / max(w, h))
        let W = Int(w * s), H = Int(h * s)
        guard let ctx = CGContext(data: nil, width: W, height: H, bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        // Released mode renders on a clear background; give the agent a solid one.
        ctx.setFillColor(CGColor(gray: 0.13, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: W, height: H))
        ctx.interpolationQuality = .high
        ctx.draw(cg, in: CGRect(x: 0, y: 0, width: W, height: H))
        guard let out = ctx.makeImage() else { return nil }
        return NSBitmapImageRep(cgImage: out).representation(using: .png, properties: [:])
    }
}

// MARK: - overlay

/// The agent's labels, drawn over the viewport with a leader to each part.
/// Re-projected every frame so they follow pose, explode and camera.
struct AgentAnnotationOverlay: View {
    let model: EditorModel

    var body: some View {
        if model.agentAnnotations.isEmpty {
            EmptyView()
        } else {
            TimelineView(.animation) { _ in
                GeometryReader { _ in
                    ZStack(alignment: .topLeading) {
                        ForEach(model.agentAnnotations) { a in
                            if let p = Self.anchor(a.instance) {
                                AnnotationPin(text: a.text, at: p)
                            }
                        }
                    }
                }
            }
            .allowsHitTesting(false)
        }
    }

    /// Screen point of an instance's visual centre, in overlay coordinates.
    static func anchor(_ instance: Int) -> CGPoint? {
        let rw = ReleaseWindowController.shared
        guard let ar = rw.arView,
              let e = rw.centeringEntity?.findEntity(named: "inst\(instance)") else { return nil }
        let c = e.visualBounds(relativeTo: nil).center
        guard let p = ar.project(c) else { return nil }
        // ARView on macOS projects in AppKit view coordinates (origin bottom-left).
        return CGPoint(x: p.x, y: ar.isFlipped ? p.y : ar.bounds.height - p.y)
    }
}

private struct AnnotationPin: View {
    let text: String
    let at: CGPoint
    private let offset = CGSize(width: 70, height: -54)

    var body: some View {
        let label = CGPoint(x: at.x + offset.width, y: at.y + offset.height)
        ZStack(alignment: .topLeading) {
            Path { p in
                p.move(to: at)
                p.addLine(to: CGPoint(x: label.x, y: label.y + 12))
            }
            .stroke(Color.orange, lineWidth: 1.5)
            Circle().fill(Color.orange).frame(width: 8, height: 8)
                .position(at)
            HStack(spacing: 6) {
                Image(systemName: "sparkle").font(.caption2)
                Text(text).font(.callout.weight(.medium)).fixedSize()
            }
            .padding(.horizontal, 10).padding(.vertical, 6)
            .background(.ultraThinMaterial, in: Capsule())
            .overlay(Capsule().strokeBorder(Color.orange.opacity(0.8), lineWidth: 1))
            .fixedSize()
            .position(x: label.x, y: label.y)
            .alignmentGuide(.leading) { $0[.leading] }
        }
    }
}
