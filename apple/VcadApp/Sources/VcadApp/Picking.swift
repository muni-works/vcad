import Foundation
import simd
import CVcadFFI
import Observation
import SwiftUI

// Public topology-picking API for the design workspace.
//
// One place answers "which face / edge of which part is under this ray, and
// which ones has the user chosen". Modeling ops (fillet on chosen edges, shell
// with chosen faces removed, sketch-on-face) and the analysis lanes (FEA load
// and support picking, DFM callouts) all read the same selection, so the ids
// mean the same thing everywhere:
//
//   face id = ordinal in the part's outer shell (vcad_scene_part_face_ids)
//   edge id = ordinal from vcad_topology_json
//
// Ids are SESSION handles: any edit that changes the B-rep renumbers them, so
// the selection is cleared whenever the document's bytes change. Persist a
// selection through `TopoEdge.selector` (an IR EdgeQuery) or
// `TopoFace.selector` (an IR FaceRef), never the raw id.

/// What a viewport click picks.
enum PickFilter: String, CaseIterable, Identifiable {
    /// Whole parts (the classic selection).
    case body
    /// Faces only.
    case face
    /// Edges only.
    case edge
    /// Whichever is nearer: an edge within tolerance, else the face.
    case faceOrEdge

    var id: String { rawValue }
    var label: String {
        switch self {
        case .body: "Body"
        case .face: "Face"
        case .edge: "Edge"
        case .faceOrEdge: "Face or Edge"
        }
    }
    var picksTopology: Bool { self != .body }

    /// Out of the box the viewport picks like Fusion 360: hovering
    /// pre-highlights the face or edge under the cursor (edges win within the
    /// pick tolerance) and a click selects it. Bodies are picked in the
    /// browser, by double-click, or by switching the Select menu to Body.
    static let standard: PickFilter = .faceOrEdge
}

/// A face or edge of one part.
struct TopoRef: Hashable, CustomStringConvertible {
    enum Kind: String, Hashable { case face, edge }
    var part: Int
    var kind: Kind
    var id: UInt32

    static func face(_ id: UInt32, part: Int) -> TopoRef { .init(part: part, kind: .face, id: id) }
    static func edge(_ id: UInt32, part: Int) -> TopoRef { .init(part: part, kind: .edge, id: id) }
    var description: String { "\(kind.rawValue) \(id) of part \(part)" }
}

/// One face from `vcad_topology_json`.
struct TopoFace: Decodable, Hashable {
    var id: UInt32
    var surface: String
    var center: [Double]
    var normal: [Double]
    var area: Double
    /// Durable IR FaceRef re-finding this face after regeneration: the
    /// persistent name when the solid carries one, plus a point on the face
    /// and its outward normal.
    var selector: JSONValue?

    // A face is its id within the part's topology; JSONValue isn't Hashable,
    // so identity is by id rather than synthesized over every field.
    static func == (a: TopoFace, b: TopoFace) -> Bool { a.id == b.id }
    func hash(into h: inout Hasher) { h.combine(id) }

    var centerF: SIMD3<Float> { Self.v(center) }
    var normalF: SIMD3<Float> { Self.v(normal) }
    /// A flat face with a usable normal; a zero normal would normalize to NaN.
    var isPlanar: Bool {
        (surface.lowercased() == "plane" || surface.lowercased() == "planar") && normal.contains { $0 != 0 }
    }
    static func v(_ a: [Double]) -> SIMD3<Float> {
        a.count >= 3 ? SIMD3(Float(a[0]), Float(a[1]), Float(a[2])) : .zero
    }
}

/// One edge from `vcad_topology_json`.
struct TopoEdge: Decodable {
    var id: UInt32
    var faces: [UInt32]
    /// Highlight polyline as segment pairs, x0 y0 z0 x1 y1 z1 … (kernel mm).
    var segments: [Float]
    var planar: Bool?
    /// Durable IR EdgeQuery naming this edge, when the solid carries names.
    var selector: JSONValue?

    /// The polyline as point pairs, ready for a ribbon.
    var segmentPoints: [SIMD3<Float>] {
        // `to: count - 2` keeps the final point. It was `count - 5`, which for
        // a straight edge (one pair, six floats) yielded a single point, so
        // the ribbon had no segment and picked edges never lit up.
        stride(from: 0, to: segments.count - 2, by: 3).map { i in
            SIMD3(segments[i], segments[i + 1], segments[i + 2])
        }
    }
    var midpoint: SIMD3<Float> {
        let p = segmentPoints
        guard !p.isEmpty else { return .zero }
        return p.reduce(.zero, +) / Float(p.count)
    }
    var length: Float {
        let p = segmentPoints
        return stride(from: 0, to: p.count - 1, by: 2).reduce(0) { $0 + simd_distance(p[$1], p[$1 + 1]) }
    }
}

extension JSONValue {
    /// Foundation form, for splicing into a `[String: Any]` document.
    var any: Any {
        switch self {
        case .null: NSNull()
        case .bool(let b): b
        case .number(let n): n
        case .string(let s): s
        case .array(let a): a.map(\.any)
        case .object(let o): o.mapValues(\.any)
        }
    }
}

/// A ray hit on a part's B-rep.
struct TopoHit {
    var ref: TopoRef
    /// The face the ray landed on (also set when `ref` is an edge).
    var face: UInt32
    var point: SIMD3<Float>
    var normal: SIMD3<Float>
    var t: Float
}

/// A part's B-rep, owned (freed on deinit), plus its topology table.
final class PartTopology {
    let solid: OpaquePointer
    let faces: [TopoFace]
    let edges: [TopoEdge]

    init?(solid: OpaquePointer) {
        guard let raw = vcad_topology_json(solid) else { vcad_solid_free(solid); return nil }
        defer { vcad_string_free(raw) }
        struct Report: Decodable { var faces: [TopoFace]; var edges: [TopoEdge] }
        guard let r = try? JSONDecoder().decode(Report.self, from: Data(String(cString: raw).utf8)) else {
            vcad_solid_free(solid); return nil
        }
        self.solid = solid
        self.faces = r.faces
        self.edges = r.edges
        // Build the pick index (topology + BVH) here: topologies are made on
        // the frame queue, so the first hover pick doesn't pay for it on main.
        _ = vcad_solid_pick_prepare(solid)
    }
    deinit { vcad_solid_free(solid) }

    func face(_ id: UInt32) -> TopoFace? { faces.first { $0.id == id } }
    func edge(_ id: UInt32) -> TopoEdge? { edges.first { $0.id == id } }

    /// Kernel ray-pick. `edgeTol` is in mm.
    func pick(origin o: SIMD3<Float>, dir d: SIMD3<Float>, edgeTol: Double) -> VcadPick {
        var oo = [Double(o.x), Double(o.y), Double(o.z)]
        var dd = [Double(d.x), Double(d.y), Double(d.z)]
        return vcad_solid_pick(solid, &oo, &dd, edgeTol)
    }
}

/// Selection + hover state for faces and edges. Owned by `EditorModel.picking`.
@MainActor
@Observable
final class PickingState {
    /// What a click in the viewport picks.
    var filter: PickFilter = .standard {
        didSet { if filter != oldValue, !filter.picksTopology { clear() } }
    }
    /// Chosen faces/edges, in pick order (ops that care about order — the
    /// first face of a shell, the path edge of a sweep — read it in order).
    private(set) var selection: [TopoRef] = []
    /// The face/edge under the pointer.
    private(set) var hovered: TopoRef?
    /// Where each selected ref was clicked (measure reads these).
    private(set) var hits: [TopoRef: TopoHit] = [:]
    /// Bumped on every selection or hover change: the viewport redraws its
    /// highlight overlay when this moves.
    private(set) var revision = 0

    /// Callbacks fired on every hover change (FEA load picking, status bar).
    /// Keyed so a panel can unregister when it goes away.
    @ObservationIgnored private var hoverHandlers: [UUID: (TopoRef?) -> Void] = [:]
    @ObservationIgnored private var selectionHandlers: [UUID: ([TopoRef]) -> Void] = [:]

    /// Per-part B-rep, keyed by the document bytes they were evaluated from.
    @ObservationIgnored private var cache: [Int: PartTopology] = [:]
    @ObservationIgnored private var cacheKey: Data?
    @ObservationIgnored nonisolated(unsafe) private var cachedScene: OpaquePointer?
    @ObservationIgnored private var prewarming: Data?

    deinit { if let s = cachedScene { vcad_scene_free(s) } }

    /// Scenes come from the document runtime's frames (`adopt`): never
    /// evaluate here. Until a revision's frame lands, picks miss.
    @ObservationIgnored var runtimeScenes: (() -> OpaquePointer?)?

    /// Parts with a B-rep ready to pick (tests, diagnostics).
    var readyPartCount: Int { cache.count }

    // MARK: Reading the selection

    var selectedFaces: [TopoRef] { selection.filter { $0.kind == .face } }
    var selectedEdges: [TopoRef] { selection.filter { $0.kind == .edge } }

    /// Face ids chosen on `part`, in pick order.
    func selectedFaceIDs(part: Int) -> [UInt32] {
        selection.compactMap { $0.part == part && $0.kind == .face ? $0.id : nil }
    }
    /// Edge ids chosen on `part`, in pick order.
    func selectedEdgeIDs(part: Int) -> [UInt32] {
        selection.compactMap { $0.part == part && $0.kind == .edge ? $0.id : nil }
    }
    /// The single part every selected entity belongs to, or nil when mixed/empty.
    var selectionPart: Int? {
        let parts = Set(selection.map(\.part))
        return parts.count == 1 ? parts.first : nil
    }
    func isSelected(_ r: TopoRef) -> Bool { selection.contains(r) }

    // MARK: Changing it

    func select(_ r: TopoRef?, additive: Bool) {
        var next = additive ? selection : []
        if let r {
            if let i = next.firstIndex(of: r) { if additive { next.remove(at: i) } }
            else { next.append(r) }
        }
        setSelection(next)
    }
    /// Select from a viewport hit, remembering where it was clicked.
    func select(hit: TopoHit?, additive: Bool) {
        if let hit { hits[hit.ref] = hit }
        select(hit?.ref, additive: additive)
    }
    func setSelection(_ refs: [TopoRef]) {
        guard refs != selection else { return }
        selection = refs
        hits = hits.filter { refs.contains($0.key) }
        revision &+= 1
        for h in selectionHandlers.values { h(refs) }
    }
    func clear() { setSelection([]); setHovered(nil) }

    func setHovered(_ r: TopoRef?) {
        guard r != hovered else { return }
        hovered = r
        revision &+= 1
        for h in hoverHandlers.values { h(r) }
    }

    /// Register a hover callback; keep the token to remove it.
    @discardableResult
    func onHover(_ handler: @escaping (TopoRef?) -> Void) -> UUID {
        let id = UUID(); hoverHandlers[id] = handler; return id
    }
    @discardableResult
    func onSelectionChange(_ handler: @escaping ([TopoRef]) -> Void) -> UUID {
        let id = UUID(); selectionHandlers[id] = handler; return id
    }
    func removeHandler(_ id: UUID) {
        hoverHandlers[id] = nil; selectionHandlers[id] = nil
    }

    // MARK: Topology

    /// Take the document runtime's evaluated scene and the topologies built
    /// from it off main (DocumentRuntime.swift) as the B-reps for `data` —
    /// picking never evaluates a document the runtime already evaluated.
    /// Takes ownership of `scene`.
    /// `sameDocument`: the scene is of the document already being picked (a
    /// frame for a revision the editor already shows) — keep the selection.
    func adopt(scene: OpaquePointer, topologies: [Int: PartTopology], data: Data, sameDocument: Bool = false) {
        if sameDocument { cacheKey = data } else { documentChanged(data) }
        if let s = cachedScene { vcad_scene_free(s) }
        cachedScene = scene
        cache = topologies
        prewarming = nil
    }

    /// Drop cached B-reps and the selection when the document changes.
    func documentChanged(_ data: Data?) {
        guard data != cacheKey else { return }
        cache.removeAll()
        if let s = cachedScene { vcad_scene_free(s); cachedScene = nil }
        cacheKey = data
        if !selection.isEmpty || hovered != nil { clear() }
    }

    private nonisolated(unsafe) static let session = EvalSession()   // guarded by sessionLock
    private nonisolated static let sessionLock = NSLock()

    /// One incremental session for every off-main B-rep consumer (picking,
    /// analysis face meshes), so the same bytes are only evaluated once.
    nonisolated static func evaluate(_ data: Data) -> OpaquePointer? {
        sessionLock.lock(); defer { sessionLock.unlock() }
        return session.scene(data: data, dir: [])
    }

    /// Evaluate the document's B-reps for picking off the main thread, so
    /// the first hover/marquee after a change doesn't block on it.
    func prewarm(_ data: Data) {
        documentChanged(data)
        guard runtimeScenes == nil, cachedScene == nil, prewarming != data else { return }
        prewarming = data
        Task.detached(priority: .background) {
            // Let the viewport's own evaluation of this revision go first:
            // three concurrent full evaluations made launch crawl.
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            guard await MainActor.run(body: { self.cacheKey == data }) else {
                await MainActor.run { if self.prewarming == data { self.prewarming = nil } }
                return
            }
            // Evaluate AND decode every part's topology here: the JSON decode
            // (`PartTopology.init`) cost ~2 s on the first hover otherwise.
            final class Box: @unchecked Sendable { var topos: [Int: PartTopology] = [:] }
            let box = Box()
            let scene = Self.evaluate(data)
            if let scene {
                for i in 0..<vcad_scene_part_count(scene) {
                    if let solid = vcad_scene_part_solid(scene, i), let t = PartTopology(solid: solid) { box.topos[i] = t }
                }
            }
            let raw = scene.map { UInt(bitPattern: $0) }
            await MainActor.run {
                let me = self
                me.prewarming = nil
                guard let raw, let ptr = OpaquePointer(bitPattern: raw) else { return }
                if me.cacheKey == data, me.cachedScene == nil {
                    me.cachedScene = ptr
                    for (i, t) in box.topos where me.cache[i] == nil { me.cache[i] = t }
                } else {
                    vcad_scene_free(ptr)
                }
            }
        }
    }

    /// The B-rep of `part` evaluated from `data` (cached until the bytes change).
    /// Nil for mesh-only parts (imports, cache-only roots).
    func topology(part: Int, document data: Data) -> PartTopology? {
        documentChanged(data)
        if let t = cache[part] { return t }
        if cachedScene == nil {
            // The document runtime's own incremental cache, when it owns the
            // document (no second evaluation session); a frame replaces it.
            cachedScene = runtimeScenes.map { $0() } ?? nil
        }
        if cachedScene == nil, runtimeScenes == nil {
            // Incremental session: after an edit only changed nodes
            // re-evaluate (a fresh vcad_scene_from_json re-did the whole
            // assembly on the main thread — seconds — on the first hover or
            // marquee after every change). `prewarm` usually got here first.
            cachedScene = Self.evaluate(data)
        }
        guard let scene = cachedScene,
              part >= 0, part < vcad_scene_part_count(scene),
              let solid = vcad_scene_part_solid(scene, part),
              let t = PartTopology(solid: solid) else { return nil }
        cache[part] = t
        return t
    }

    /// Ray-pick `part` under the current filter. `edgeTol` in mm.
    func pick(part: Int, document data: Data,
              origin: SIMD3<Float>, dir: SIMD3<Float>, edgeTol: Double) -> TopoHit? {
        guard filter.picksTopology,
              let topo = topology(part: part, document: data) else { return nil }
        let p = topo.pick(origin: origin, dir: dir, edgeTol: filter == .face ? 0 : edgeTol)
        guard p.hit != 0 else { return nil }
        let point = SIMD3(Float(p.point.0), Float(p.point.1), Float(p.point.2))
        let normal = SIMD3(Float(p.normal.0), Float(p.normal.1), Float(p.normal.2))
        let ref: TopoRef?
        switch filter {
        case .face: ref = p.face == VCAD_TOPO_NONE ? nil : .face(p.face, part: part)
        case .edge: ref = p.edge == VCAD_TOPO_NONE ? nil : .edge(p.edge, part: part)
        case .faceOrEdge:
            ref = p.edge != VCAD_TOPO_NONE ? .edge(p.edge, part: part)
                : p.face != VCAD_TOPO_NONE ? .face(p.face, part: part) : nil
        case .body: ref = nil
        }
        guard let ref else { return nil }
        return TopoHit(ref: ref, face: p.face, point: point, normal: normal, t: Float(p.t))
    }

    /// Highlight polylines (segment point pairs) for a set of refs.
    func highlightSegments(_ refs: [TopoRef], document data: Data) -> [SIMD3<Float>] {
        var out: [SIMD3<Float>] = []
        for r in refs {
            guard let t = topology(part: r.part, document: data) else { continue }
            switch r.kind {
            case .edge:
                if let e = t.edge(r.id) { out += e.segmentPoints }
            case .face:
                // A face reads as its boundary: every edge that bounds it.
                for e in t.edges where e.faces.contains(r.id) { out += e.segmentPoints }
            }
        }
        return out
    }

    /// Durable selectors for the chosen edges of `part` (nil entries dropped).
    func edgeSelectors(part: Int, document data: Data) -> [JSONValue] {
        guard let t = topology(part: part, document: data) else { return [] }
        return selectedEdgeIDs(part: part).compactMap { t.edge($0)?.selector }
    }
}

extension EditorModel {
    /// Kick a background B-rep evaluation for picking (call on document change).
    func prewarmPicking() {
        if let data = pickingDocumentData { picking.prewarm(data) }
    }

    /// The open document's bytes, as the topology picker keys them. Nil when
    /// there is no editable document.
    var pickingDocumentData: Data? {
        if let c = documentBytesCache, c.revision == documentRevision { return c.data }
        guard let doc = documentJSON,
              let data = try? JSONSerialization.data(withJSONObject: doc, options: [.sortedKeys]) else { return nil }
        documentBytesCache = (documentRevision, data)
        return data
    }

    /// Pick a face/edge of `part` along a kernel-space ray. With the camera's
    /// distance (mm) and the viewport height (points), the edge tolerance is
    /// about 6 px on screen at any zoom; without them it falls back to a
    /// scene-relative world tolerance.
    func pickTopology(part: Int, origin: SIMD3<Float>, dir: SIMD3<Float>,
                      cameraDistance: Float? = nil, viewportHeight: Float? = nil) -> TopoHit? {
        guard let data = pickingDocumentData else { return nil }
        let tol: Double
        if let d = cameraDistance, let h = viewportHeight, d > 0, h > 0 {
            tol = SelectionMath.edgePickTolerance(distance: d, viewportHeight: h)
        } else {
            tol = max(Double(displayedSceneSize) * 0.02, 0.05)
        }
        return picking.pick(part: part, document: data, origin: origin, dir: dir, edgeTol: tol)
    }

    /// Topology table of `part` (faces with centers/normals, edges with
    /// polylines and selectors), or nil for mesh-only parts.
    func partTopology(_ part: Int) -> PartTopology? {
        guard let data = pickingDocumentData else { return nil }
        return picking.topology(part: part, document: data)
    }

    /// Highlight polylines for the current selection and hover.
    func topologyHighlight() -> (selected: [SIMD3<Float>], hovered: [SIMD3<Float>]) {
        guard let data = pickingDocumentData, picking.filter.picksTopology else { return ([], []) }
        let sel = picking.highlightSegments(picking.selection, document: data)
        let hov = picking.hovered.map { picking.highlightSegments([$0], document: data) } ?? []
        return (sel, hov)
    }
}

extension EditorModel {
    /// "2 faces · 1 240 mm²" / "1 edge · 42.0 mm" for the topology selection.
    func topologySelectionReadout() -> String? {
        guard picking.filter.picksTopology, !picking.selection.isEmpty,
              let data = pickingDocumentData else { return nil }
        var areas: [Double] = [], lengths: [Double] = []
        for r in picking.selection {
            guard let t = picking.topology(part: r.part, document: data) else { continue }
            switch r.kind {
            case .face: if let f = t.face(r.id) { areas.append(f.area) }
            case .edge: if let e = t.edge(r.id) { lengths.append(Double(e.length)) }
            }
        }
        return SelectionMath.readout(faceAreas: areas, edgeLengths: lengths)
    }

    /// Kernel-space centres of the selected faces (centroid) and edges
    /// (midpoint) — the orbit pivot candidates.
    func topologySelectionCenters() -> [SIMD3<Float>] {
        guard picking.filter.picksTopology, !picking.selection.isEmpty,
              let data = pickingDocumentData else { return [] }
        return picking.selection.compactMap { r in
            guard let t = picking.topology(part: r.part, document: data) else { return nil }
            switch r.kind {
            case .face: return t.face(r.id)?.centerF
            case .edge: return t.edge(r.id)?.midpoint
            }
        }
    }
}

extension PickFilter {
    /// ⌃-shortcut in the Select menu.
    var shortcut: KeyEquivalent {
        switch self {
        case .body: "b"
        case .face: "f"
        case .edge: "e"
        case .faceOrEdge: "a"
        }
    }
}
