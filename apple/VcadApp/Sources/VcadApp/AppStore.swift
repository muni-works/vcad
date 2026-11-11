import Foundation
import simd

// MCP-native M1: the live open document as an MCP document store.
//
// `DocumentStore` mirrors the Rust trait M0 is landing in `crates/vcad-mcp`
// (HeadlessStore = session files, AppStore = this window). Until that crate
// is on main and reachable through `vcad-ffi`, the app's Swift MCP server
// (AgentBridge.swift) speaks to this protocol directly; when the Rust core
// arrives, `AppStore` becomes the FFI callback table behind it and nothing
// above this file changes.

/// One resource the store publishes. URIs are stable across the session.
enum StoreResource: String, CaseIterable {
    case document = "vcad://doc/current"
    case selection = "vcad://doc/selection"
    case view = "vcad://doc/view"

    var name: String {
        switch self {
        case .document: "Open document"
        case .selection: "User selection"
        case .view: "Viewport camera"
        }
    }
    var description: String {
        switch self {
        case .document: "The document open in vcad.app (IR JSON). Updates on every edit — yours or the user's."
        case .selection: "What the user has selected in the viewport: parts, faces and edges (with durable selectors). \"This edge\" means this."
        case .view: "The viewport camera (azimuth/elevation in degrees, distance) and workspace."
        }
    }
    /// The `vcad://doc/<x>` short forms agents sometimes write.
    static func parse(_ uri: String) -> StoreResource? {
        if let r = StoreResource(rawValue: uri) { return r }
        switch uri {
        case "vcad://doc/current/", "vcad://current": return .document
        case "vcad://selection": return .selection
        case "vcad://view": return .view
        default: return nil
        }
    }
}

/// Swift mirror of `vcad_mcp::DocumentStore`.
@MainActor
protocol DocumentStore: AnyObject {
    /// Opaque revision; changes iff the document changed.
    var revision: String { get }
    /// The document as IR JSON.
    func document() -> [String: Any]
    func selection() -> [String: Any]
    func view() -> [String: Any]
    /// Changes iff `read(r)` would read differently. Polled ~10×/s.
    func fingerprint(_ r: StoreResource) -> String
    /// Run `body` against the editable document as one named step of the
    /// current agent turn. Throws (and leaves the document untouched) on failure.
    func apply(name: String, _ body: (DocumentEngine) throws -> DocumentEngine.Result) throws -> DocumentEngine.Result
}

extension DocumentStore {
    /// Fingerprint per resource: a subscriber is notified when it moves.
    /// Stores override this with something cheaper than a full read.
    func defaultFingerprint(_ r: StoreResource) -> String {
        switch r {
        case .document: return revision
        case .selection: return Self.stableJSON(selection())
        case .view: return Self.stableJSON(view())
        }
    }
    func read(_ r: StoreResource) -> [String: Any] {
        switch r {
        case .document: return document()
        case .selection: return selection()
        case .view: return view()
        }
    }
    static func stableJSON(_ o: Any) -> String {
        let d = (try? JSONSerialization.data(withJSONObject: o, options: [.sortedKeys])) ?? Data()
        return String(decoding: d, as: UTF8.self)
    }
}

/// The live window's document.
@MainActor
final class AppStore: DocumentStore {
    let model: EditorModel
    init(model: EditorModel) { self.model = model }

    var revision: String {
        if let e = model.engine { return e.stateHash }
        return model.documentJSON.map { Self.stableJSON($0).hashValue.description } ?? "empty"
    }

    func document() -> [String: Any] {
        var d: [String: Any] = ["name": model.documentName, "revision": revision,
                                "editable": model.agentCanEdit]
        if let p = model.documentURL?.path { d["path"] = p }
        d["document"] = model.documentJSON ?? NSNull()
        return d
    }

    func selection() -> [String: Any] {
        let ids = model.assemblyInstanceIDs
        var parts: [Any] = []
        if model.usesDocumentTree {
            let chosen = !model.multiSelectedParts.isEmpty ? model.multiSelectedParts
                : model.selectedPartIndex.map { [$0] } ?? []
            for i in chosen.sorted() {
                parts.append(["index": i, "part_id": model.partId(forPart: i) ?? NSNull()] as [String: Any])
            }
        }
        let instances = model.selectedInstances.sorted().map { ids.indices.contains($0) ? ids[$0] : "\($0)" }
        let topo = model.picking.selection.map { r -> [String: Any] in
            var o: [String: Any] = ["kind": r.kind == .edge ? "edge" : "face", "part": r.part, "id": Int(r.id),
                                    "part_id": model.partId(forPart: r.part) ?? NSNull()]
            if let t = model.partTopology(r.part) {
                let sel = r.kind == .edge ? t.edge(r.id)?.selector : t.face(r.id)?.selector
                if let sel { o["selector"] = sel.any }
            }
            return o
        }
        return ["parts": parts, "instances": instances, "topology": topo,
                "feature": model.selectedFeatureID ?? NSNull()]
    }

    func view() -> [String: Any] {
        var v = AgentTools.camera(model)
        v["workspace"] = "\(model.workspace)"
        // Round so idle float jitter never reads as a change.
        for k in ["azimuth", "elevation", "distance"] {
            if let x = v[k] as? Double { v[k] = (x * 100).rounded() / 100 }
        }
        return v
    }

    func fingerprint(_ r: StoreResource) -> String {
        switch r {
        case .document: return revision
        // Selection is read lazily (selectors need the B-rep); its
        // fingerprint is the inputs, not the output.
        case .selection:
            return "\(model.picking.revision)|\(model.selectedFeatureID ?? "")|\(model.multiSelectedParts)|\(model.selectedInstances.sorted())"
        case .view: return Self.stableJSON(view())
        }
    }

    func apply(name: String, _ body: (DocumentEngine) throws -> DocumentEngine.Result) throws -> DocumentEngine.Result {
        try AgentEdits.edit(model, name: name, body)
    }
}
