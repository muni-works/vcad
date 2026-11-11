#if canImport(AppKit)
import AppKit
#endif
import Foundation
import Network

// MCP resources for the live window (MCP-native M1):
//
//   vcad://doc/current    the open document (IR JSON + revision)
//   vcad://doc/selection  what the user picked — parts, faces, edges + selectors
//   vcad://doc/view       the viewport camera
//   ui://vcad/viewer      MCP Apps viewer, rendered from the live viewport
//
// Subscriptions: `resources/subscribe`, then `notifications/resources/updated`
// arrive on the server→client SSE stream (`GET /mcp`, streamable HTTP).
// Change detection polls each store's cheap fingerprint at 10 Hz while anyone
// is subscribed — the app's own edits, the user's clicks and the agent's
// edits all surface the same way, with no hooks threaded through the editor.

@MainActor
final class AgentResources {
    static let shared = AgentResources()

    static let viewerURI = "ui://vcad/viewer"

    var store: (any DocumentStore)?
    private(set) var subscribed: Set<StoreResource> = []
    private var fingerprints: [StoreResource: String] = [:]
    private var streams: [ObjectIdentifier: NWConnection] = [:]
    private var timer: Timer?
    /// Test seam: every notification sent, in order.
    var sent: [[String: Any]] = []

    // MARK: protocol

    func list() -> [String: Any] {
        var rs: [[String: Any]] = StoreResource.allCases.map {
            ["uri": $0.rawValue, "name": $0.name, "description": $0.description, "mimeType": "application/json"]
        }
        rs.append(["uri": Self.viewerURI, "name": "vcad viewer",
                   "description": "Live view of the open vcad window (MCP Apps).",
                   "mimeType": "text/html;profile=mcp-app"])
        return ["resources": rs]
    }

    func read(_ uri: String) throws -> [String: Any] {
        if uri == Self.viewerURI {
            return ["contents": [["uri": uri, "mimeType": "text/html;profile=mcp-app", "text": AgentViewer.html]]]
        }
        guard let r = StoreResource.parse(uri) else { throw AgentTools.Failure(message: "unknown resource \(uri)") }
        guard let store else { throw AgentTools.Failure(message: "The app has no window yet.") }
        let data = (try? JSONSerialization.data(withJSONObject: store.read(r), options: [.sortedKeys])) ?? Data()
        return ["contents": [["uri": r.rawValue, "mimeType": "application/json",
                              "text": String(decoding: data, as: UTF8.self)]]]
    }

    func subscribe(_ uri: String) throws {
        guard let r = StoreResource.parse(uri) else { throw AgentTools.Failure(message: "unknown resource \(uri)") }
        subscribed.insert(r)
        if let store { fingerprints[r] = store.fingerprint(r) }
        startPolling()
    }

    func unsubscribe(_ uri: String) {
        guard let r = StoreResource.parse(uri) else { return }
        subscribed.remove(r)
        fingerprints[r] = nil
        if subscribed.isEmpty { timer?.invalidate(); timer = nil }
    }

    // MARK: change detection

    private func startPolling() {
        guard timer == nil else { return }
        timer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { _ in
            MainActor.assumeIsolated { AgentResources.shared.poll() }
        }
    }

    /// Compare fingerprints; notify subscribers of whatever moved.
    func poll() {
        guard let store else { return }
        for r in StoreResource.allCases where subscribed.contains(r) {
            let f = store.fingerprint(r)
            if fingerprints[r] != f {
                fingerprints[r] = f
                notify(["jsonrpc": "2.0", "method": "notifications/resources/updated",
                        "params": ["uri": r.rawValue]])
            }
        }
    }

    // MARK: SSE

    /// Hold `conn` open as a server→client event stream.
    func attachStream(_ conn: NWConnection) {
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n"
            + "Connection: keep-alive\r\nMcp-Session-Id: vcad-app\r\n\r\n: vcad\n\n"
        let id = ObjectIdentifier(conn)
        streams[id] = conn
        conn.stateUpdateHandler = { state in
            switch state {
            case .failed, .cancelled:
                MainActor.assumeIsolated { _ = AgentResources.shared.streams.removeValue(forKey: id) }
            default: break
            }
        }
        conn.send(content: Data(head.utf8), completion: .contentProcessed { _ in })
    }

    func notify(_ msg: [String: Any]) {
        sent.append(msg)
        if sent.count > 256 { sent.removeFirst(sent.count - 256) }
        guard let data = try? JSONSerialization.data(withJSONObject: msg) else { return }
        let frame = Data("event: message\ndata: ".utf8) + data + Data("\n\n".utf8)
        for (id, c) in streams {
            c.send(content: frame, completion: .contentProcessed { err in
                guard err != nil else { return }
                MainActor.assumeIsolated { AgentResources.shared.streams[id]?.cancel() }
            })
        }
    }
}

// MARK: - viewer + focus tools

@MainActor
enum AgentViewer {
    /// Tools with a live viewer: mutating ones carry `_meta.ui.resourceUri`.
    static let meta: [String: Any] = ["ui": ["resourceUri": AgentResources.viewerURI]]

    static let definitions: [[String: Any]] = [
        ["name": "viewer_frame",
         "description": "The live vcad viewport as a PNG plus the document revision. Used by the vcad viewer app; agents should prefer snapshot.",
         "inputSchema": ["type": "object", "properties": [String: Any]()] as [String: Any]],
        ["name": "open_in_app",
         "description": "Bring the vcad window showing this document to the front. Focuses the existing window — never opens a copy.",
         "inputSchema": ["type": "object", "properties": [String: Any]()] as [String: Any]],
        ["name": "select_edges",
         "description": "Select edges in the user's viewport, as if they clicked them: part index plus edge ids (from vcad://doc/selection or describe_scene), or edge:\"longest\". The v2 agent-cursor seam (agent drives selection); also what demo scripts use in place of a click.",
         "inputSchema": ["type": "object",
                         "properties": ["part": ["type": "integer"],
                                        "edges": ["type": "array", "items": ["type": "integer"]],
                                        "edge": ["type": "string", "enum": ["longest"]]] as [String: Any],
                         "required": ["part"]] as [String: Any]],
        ["name": "fillet_edges",
         "description": "Fillet (or chamfer) the edges the user selected in the viewport — read vcad://doc/selection. With nothing selected, every edge of the selected part. One undo step, live.",
         "inputSchema": ["type": "object",
                         "properties": ["radius": ["type": "number", "description": "mm"],
                                        "chamfer": ["type": "boolean"]] as [String: Any],
                         "required": ["radius"]] as [String: Any],
         "_meta": meta],
    ]
    static var toolNames: Set<String> { Set(definitions.compactMap { $0["name"] as? String }) }

    static func call(_ name: String, _ a: [String: Any], model m: EditorModel) async throws -> [String: Any] {
        switch name {
        case "viewer_frame":
            let png = await AgentTools.snapshotPNG()
            var content: [[String: Any]] = [["type": "text", "text": AgentStoreText.summary(m)]]
            if let png { content.insert(["type": "image", "data": png.base64EncodedString(), "mimeType": "image/png"], at: 0) }
            return ["content": content,
                    "structuredContent": ["revision": AgentResources.shared.store?.revision ?? "",
                                          "name": m.documentName]]
        case "open_in_app":
            focusWindow()
            return AgentTools.text("vcad is in front, showing \(m.documentName).")
        case "fillet_edges":
            return try filletEdges(a, m)
        case "select_edges":
            let part = (a["part"] as? NSNumber)?.intValue ?? 0
            guard let topo = m.partTopology(part) else { throw AgentTools.Failure(message: "part \(part) has no B-rep") }
            var ids = (a["edges"] as? [NSNumber])?.map { UInt32($0.intValue) } ?? []
            if a["edge"] as? String == "longest", let e = topo.edges.max(by: { $0.length < $1.length }) { ids = [e.id] }
            guard !ids.isEmpty, ids.allSatisfy({ topo.edge($0) != nil }) else {
                throw AgentTools.Failure(message: "no such edges on part \(part)")
            }
            m.selectParts([part])
            m.picking.filter = .edge
            m.picking.setSelection(ids.map { .edge($0, part: part) })
            let mid = ids.compactMap { topo.edge($0)?.midpoint }.reduce(.zero, +) / Float(ids.count)
            AgentCursor.shared.move(to: .point(mid), label: "\(AgentCursor.shared.agentName) · selecting")
            return AgentTools.text("Selected \(ids.count) edge\(ids.count == 1 ? "" : "s") on part \(part).")
        default:
            throw AgentTools.Failure(message: "unknown tool \(name)")
        }
    }

    static func focusWindow() {
        #if canImport(AppKit)
        NSApp.activate(ignoringOtherApps: true)
        ReleaseWindowController.shared.hostWindow?.makeKeyAndOrderFront(nil)
        #endif
    }

    static func filletEdges(_ a: [String: Any], _ m: EditorModel) throws -> [String: Any] {
        guard let radius = (a["radius"] as? NSNumber)?.doubleValue, radius > 0 else {
            throw AgentTools.Failure(message: "radius (mm, > 0) is required")
        }
        let chamfer = a["chamfer"] as? Bool ?? false
        let edges = m.picking.selectedEdges.count
        let focus = AgentCursor.focus(m)
        let fallback = "Claude: \(chamfer ? "Chamfer" : "Fillet") \(edges == 0 ? "Part" : "Edges")"
        var ok = false
        try AgentTurn.shared.perform(m, name: fallback) {
            ok = m.applyBlend(fillet: !chamfer, size: radius)
        }
        guard ok else {
            throw AgentTools.Failure(message: m.engineError ?? "Select a part or some of its edges in the viewport first (read vcad://doc/selection).")
        }
        if let r = m.engineState {
            AgentCursor.shared.pointAt(touched: r, verb: AgentTurn.shared.undoName(fallback: fallback),
                                       focus: focus, model: m)
        }
        let what = edges == 0 ? "every edge of the part" : "\(edges) selected edge\(edges == 1 ? "" : "s")"
        return ["content": [["type": "text", "text": "\(chamfer ? "Chamfered" : "Filleted") \(what) at \(radius) mm. Undo label: \"\(m.undoActionName ?? fallback)\"."]],
                "_meta": meta]
    }
}

@MainActor
enum AgentStoreText {
    static func summary(_ m: EditorModel) -> String {
        "\(m.documentName): \(m.partCount) part\(m.partCount == 1 ? "" : "s")"
    }
}
