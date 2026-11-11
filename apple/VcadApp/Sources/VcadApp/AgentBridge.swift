#if canImport(AppKit)
import AppKit
#endif
import Foundation
import Darwin
import Network
import RealityKit
import simd
import SwiftUI

// Agent bridge: the app hosts vcad's MCP core (AgentCore.swift →
// crates/vcad-mcp) so a Claude session and the user share ONE view — the
// release-to-desktop viewport. The agent reads what the user is pointing at
// (selection / hover), drives pose, explode and camera, lights parts and pins
// labels the user sees, and takes snapshots of exactly what is on screen.
//
// The core owns the protocol, the 127.0.0.1 endpoint and every tool's listing
// and dispatch; the viewport tools below are host tools it calls back into
// (`AgentTools.call`). Document edits are native in the core and land here as
// one named undo step each (AgentCore `apply` / `engine_tool`).
//
// Transport: MCP streamable HTTP at http://127.0.0.1:<port>/mcp (served by the
// core). The port and pid land in ~/.vcad/instances/<pid>.json (+ the
// ~/.vcad/app.json "most recent" pointer) for `vcad mcp`, the stdio shim
// (apple/VcadApp/scripts/vcad-app-mcp.mjs) and @vcad/mcp. Loopback only.

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
    private(set) var port: UInt16?

    /// Host the MCP core (AgentCore.swift → crates/vcad-mcp) on 127.0.0.1 and
    /// advertise it in ~/.vcad/instances. The core serves the protocol and
    /// the tools; this class is discovery and the instance stamp.
    func start(model: EditorModel) {
        self.model = model
        AgentResources.shared.store = AppStore(model: model)
        guard port == nil else { return }
        guard ProcessInfo.processInfo.environment["VCAD_AGENT_BRIDGE"] != "0" else { return }
        // VCAD_AGENT_PORT pins the port (a second instance during development
        // would otherwise take the next free one).
        let pinned = ProcessInfo.processInfo.environment["VCAD_AGENT_PORT"].flatMap { UInt16($0) }
        guard let p = AgentCore.shared.start(model: model, port: pinned ?? Self.basePort,
                                             attempts: pinned == nil ? 8 : 0) else { return }
        port = p
        writeDiscovery(port: p)
        AgentResources.shared.startPolling()
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
    /// how `vcad mcp`, the stdio shim and @vcad/mcp find us. See AgentDiscovery.swift.
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
        FileHandle.standardError.write(Data("[vcad] MCP core on http://127.0.0.1:\(port)/mcp\n".utf8))
    }

    func refreshDiscovery() {
        guard let r = record, r != lastRecord else { return }
        lastRecord = r
        AgentDiscovery.write(r)
    }

    /// Names this instance on every response, so an agent can tell which window it is driving.
    var instanceMeta: [String: Any] {
        refreshDiscovery()
        guard let r = record else { return [:] }
        return ["pid": r.pid, "port": r.port, "instance_id": r.instanceId, "started": r.started,
                "document": r.documentName ?? NSNull(), "document_path": r.documentPath ?? NSNull()]
    }

    /// Adds `_meta["io.vcad/instance"]` and a trailing one-line text block
    /// ("vcad.app pid 123 · bracket.vcad") to a tool result — what the core's
    /// `instance_meta` stamp does to every result (kept for in-app callers).
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
    Omit document_id to work on the open window. Generated .loon documents and electronics are view-only. \
    Resources: vcad://doc/selection is what the user picked (subscribe to follow it; "this edge" means \
    it), vcad://doc/current the live document, vcad://doc/view the camera. Before a multi-step change \
    call agent_turn {label:"thickened walls"} so it lands as ONE undo step, and agent_turn {end:true} \
    after. fillet_edges fillets the user's selected edges. Receipts (assert_clearance, verify_receipts), \
    fab_print / fab_mill (dry-run, as tasks) and optimize_parameters run in the same core.
    """
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

    /// Viewport host tools implemented in `call` (listed by the core's
    /// host_tools.json; `AgentCoreTests` checks the two agree).
    static let viewportToolNames: Set<String> = [
        "app_state", "list_parts", "set_pose", "set_explode", "set_camera", "frame_part", "highlight_parts",
        "annotate", "clear_annotations", "suggest_edit", "history_seek", "import_board", "press_key",
        "set_workspace", "select_feature", "app_screenshot", "snapshot", "photoreal",
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
            guard AgentEdits.suggestableTools.contains(tool) else {
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
        case _ where AgentEdits.hostToolNames.contains(name):
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
