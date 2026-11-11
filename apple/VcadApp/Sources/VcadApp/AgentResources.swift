#if canImport(AppKit)
import AppKit
#endif
import Foundation

// MCP resources for the live window (MCP-native M1):
//
//   vcad://doc/current    the open document (IR JSON + revision)
//   vcad://doc/selection  what the user picked — parts, faces, edges + selectors
//   vcad://doc/view       the viewport camera
//   ui://vcad/viewer      MCP Apps viewer, rendered from the live viewport
//
// Subscriptions are the MCP core's (crates/vcad-mcp): it streams
// `notifications/resources/updated` to clients (`GET /mcp`, or a modern
// `subscriptions/listen`); this file tells it what moved.
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
        startTimer()
    }

    func unsubscribe(_ uri: String) {
        guard let r = StoreResource.parse(uri) else { return }
        subscribed.remove(r)
        fingerprints[r] = nil
        if subscribed.isEmpty, !followAll { timer?.invalidate(); timer = nil }
    }

    // MARK: change detection

    /// Follow every resource (the core tracks who subscribed; it forwards
    /// updates to its clients). Started with the core.
    private var followAll = false

    func startPolling() {
        followAll = true
        if let store { for r in StoreResource.allCases { fingerprints[r] = store.fingerprint(r) } }
        startTimer()
    }

    private func startTimer() {
        guard timer == nil else { return }
        timer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { _ in
            MainActor.assumeIsolated { AgentResources.shared.poll() }
        }
    }

    /// Compare fingerprints; notify subscribers of whatever moved.
    func poll() {
        guard let store else { return }
        for r in StoreResource.allCases where followAll || subscribed.contains(r) {
            let f = store.fingerprint(r)
            if fingerprints[r] != f {
                fingerprints[r] = f
                notify(["jsonrpc": "2.0", "method": "notifications/resources/updated",
                        "params": ["uri": r.rawValue]])
            }
        }
    }

    // MARK: notifications

    /// Hand a notification to the core, which streams it to every client.
    func notify(_ msg: [String: Any]) {
        sent.append(msg)
        if sent.count > 256 { sent.removeFirst(sent.count - 256) }
        if let uri = (msg["params"] as? [String: Any])?["uri"] as? String {
            AgentCore.shared.resourceUpdated(uri)
        }
    }
}

// MARK: - viewer + focus tools

@MainActor
enum AgentViewer {
    /// Tools with a live viewer: mutating ones carry `_meta.ui.resourceUri`.
    static let meta: [String: Any] = ["ui": ["resourceUri": AgentResources.viewerURI]]
    static let toolNames: Set<String> = ["viewer_frame", "open_in_app", "select_edges", "fillet_edges"]

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
        // "Fillet this face": a picked face means its boundary edges.
        let facesPicked = m.picking.selectedFaces.count
        if m.picking.selectedEdges.isEmpty, facesPicked > 0, let part = m.picking.selectionPart,
           let topo = m.partTopology(part) {
            let faceIds = Set(m.picking.selectedFaces.map(\.id))
            let ring = topo.edges.filter { !faceIds.isDisjoint(with: $0.faces) }.map { TopoRef.edge($0.id, part: part) }
            if !ring.isEmpty { m.picking.setSelection(ring) }
        }
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
        let what = edges == 0 ? "every edge of the part"
            : facesPicked > 0 ? "the \(edges) edges around \(facesPicked) selected face\(facesPicked == 1 ? "" : "s")"
            : "\(edges) selected edge\(edges == 1 ? "" : "s")"
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
