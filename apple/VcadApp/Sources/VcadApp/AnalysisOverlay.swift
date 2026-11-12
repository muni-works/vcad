import Foundation
import RealityKit
import AppKit
import CVcadFFI
import simd

// Viewport overlay for the Analysis panel, drawn in the kernel-mm frame next
// to the field overlay:
// - DFM: offending faces tinted by severity (the focused issue brighter),
//   plus a marker at each issue's anchor;
// - FEA: faces carrying loads (orange) and supports (blue);
// - Clearance: interference solids in translucent red (web ClashMesh parity).
//
// Face tint comes from the evaluated part mesh's per-triangle face ids
// (`vcad_scene_part_face_ids`) — the same ids DFM reports and FEA picks use.

/// Per-part triangles tagged with face ids, cached per document bytes.
///
/// Evaluated **off the main thread** with its own incremental session: the
/// old synchronous `vcad_scene_from_json` re-evaluated the whole document on
/// the main thread the first time a face was selected after any edit
/// (seconds on an assembly — the app froze). Until the meshes for the
/// current bytes are ready, callers get the previous set (or none) and
/// `.analysisMeshesReady` is posted when the new one lands.
@MainActor
final class AnalysisMeshCache {
    static let shared = AnalysisMeshCache()
    struct Part: Sendable { var positions: [SIMD3<Float>]; var normals: [SIMD3<Float>]; var indices: [UInt32]; var faceIds: [UInt32] }
    private var key: Data?
    private var parts: [Part] = []
    private var pending: Data?
    private nonisolated(unsafe) let session = EvalSession()

    func parts(for data: Data) -> [Part] {
        if data == key { return parts }
        if pending != data {
            pending = data
            let session = self.session
            Task.detached(priority: .userInitiated) {
                let built = Self.build(data, session: session)
                await MainActor.run {
                    let cache = AnalysisMeshCache.shared
                    guard cache.pending == data else { return }
                    cache.key = data
                    cache.parts = built
                    cache.pending = nil
                    NotificationCenter.default.post(name: .analysisMeshesReady, object: nil)
                }
            }
        }
        return []
    }

    nonisolated private static func build(_ data: Data, session: EvalSession) -> [Part] {
        var out: [Part] = []
        guard let scene = session.scene(data: data, dir: []) else { return out }
        defer { vcad_scene_free(scene) }
        for i in 0..<vcad_scene_part_count(scene) {
            let v = vcad_scene_part_mesh(scene, i)
            var n = 0
            let ids = vcad_scene_part_face_ids(scene, i, &n)
            guard let vp = v.vertices, let ip = v.indices else { out.append(.init(positions: [], normals: [], indices: [], faceIds: [])); continue }
            let pos = (0..<(v.vertices_len / 3)).map { SIMD3(vp[3 * $0], vp[3 * $0 + 1], vp[3 * $0 + 2]) }
            let nor: [SIMD3<Float>] = (v.normals != nil && v.normals_len == v.vertices_len)
                ? (0..<(v.normals_len / 3)).map { SIMD3(v.normals![3 * $0], v.normals![3 * $0 + 1], v.normals![3 * $0 + 2]) }
                : Array(repeating: .zero, count: pos.count)
            let idx = Array(UnsafeBufferPointer(start: ip, count: v.indices_len))
            let fid = ids.map { Array(UnsafeBufferPointer(start: $0, count: n)) } ?? []
            out.append(.init(positions: pos, normals: nor, indices: idx, faceIds: fid))
        }
        return out
    }

    /// Triangles of `faces` on `part`, pushed out along their normals by `lift` mm.
    func faceMesh(part: Int, faces: Set<UInt32>, data: Data, lift: Float) -> MeshResource? {
        let ps = parts(for: data)
        guard part >= 0, part < ps.count, !faces.isEmpty else { return nil }
        let p = ps[part]
        guard p.faceIds.count == p.indices.count / 3 else { return nil }
        var pos: [SIMD3<Float>] = [], idx: [UInt32] = []
        for t in 0..<p.faceIds.count where faces.contains(p.faceIds[t]) {
            let a = p.positions[Int(p.indices[3 * t])], b = p.positions[Int(p.indices[3 * t + 1])], c = p.positions[Int(p.indices[3 * t + 2])]
            let n = simd_normalize(simd_cross(b - a, c - a))
            let off = n.x.isFinite ? n * lift : .zero
            let base = UInt32(pos.count)
            pos += [a + off, b + off, c + off]
            idx += [base, base + 1, base + 2]
        }
        guard !idx.isEmpty else { return nil }
        var d = MeshDescriptor(name: "analysisFaces")
        d.positions = MeshBuffers.Positions(pos)
        d.primitives = .triangles(idx)
        return try? MeshResource.generate(from: [d])
    }
}

extension Notification.Name {
    /// `AnalysisMeshCache` finished meshes for the current document bytes.
    static let analysisMeshesReady = Notification.Name("vcad.analysisMeshesReady")
}

@MainActor
private func tint(_ c: NSColor, _ alpha: CGFloat) -> UnlitMaterial {
    var m = UnlitMaterial(color: c.withAlphaComponent(alpha))
    m.blending = .transparent(opacity: .init(floatLiteral: Float(alpha)))
    m.faceCulling = .none
    return m
}

/// Build or update the analysis entity under `parent` (kernel-mm frame).
@MainActor
func syncAnalysisOverlay(_ store: AnalysisStore, model: EditorModel, in parent: Entity) {
    let key = "analysisRoot-\(store.overlayRevision)-\(store.tab.rawValue)-\(store.shown)-\(store.focusedIssue ?? "")"
    if parent.findEntity(named: key) != nil { return }
    parent.children.filter { $0.name.hasPrefix("analysisRoot") }.forEach { $0.removeFromParent() }
    guard store.shown, let data = AnalysisStore.documentData(model) else { return }
    let root = Entity()
    root.name = key
    let cache = AnalysisMeshCache.shared
    let lift = Float(max(model.displayedSceneSize, 1)) * 0.0015

    switch store.tab {
    case .dfm:
        guard let r = store.dfmReport, !store.dfmStale(model) else { break }
        let byPart = Dictionary(grouping: r.issues, by: \.part_index)
        for (part, issues) in byPart {
            for sev in ["info", "warning", "error"] {
                let faces = Set(issues.filter { $0.severity == sev && $0.id != store.focusedIssue }.flatMap(\.face_ids))
                let color: NSColor = sev == "error" ? .systemRed : sev == "warning" ? .systemOrange : .systemBlue
                if let mesh = cache.faceMesh(part: part, faces: faces, data: data, lift: lift) {
                    root.addChild(ModelEntity(mesh: mesh, materials: [tint(color, 0.35)]))
                }
            }
        }
        if let f = r.issues.first(where: { $0.id == store.focusedIssue }) {
            if let mesh = cache.faceMesh(part: f.part_index, faces: Set(f.face_ids), data: data, lift: lift * 2) {
                root.addChild(ModelEntity(mesh: mesh, materials: [tint(f.severity == "error" ? .systemRed : .systemOrange, 0.75)]))
            }
            let marker = ModelEntity(mesh: .generateSphere(radius: lift * 4), materials: [tint(.white, 0.95)])
            marker.position = f.anchorF
            root.addChild(marker)
        }
    case .fea:
        let part = store.feaPart
        let loads = Set(store.feaLoads.flatMap(\.faces)), supports = Set(store.feaSupports.flatMap(\.faces))
        if let mesh = cache.faceMesh(part: part, faces: supports, data: data, lift: lift) {
            root.addChild(ModelEntity(mesh: mesh, materials: [tint(.systemBlue, 0.55)]))
        }
        if let mesh = cache.faceMesh(part: part, faces: loads, data: data, lift: lift * 1.5) {
            root.addChild(ModelEntity(mesh: mesh, materials: [tint(.systemOrange, 0.65)]))
        }
    case .clearance:
        guard let r = store.clearance, !store.clearanceStale(model) else { break }
        for p in r.pairs {
            guard let m = p.clash, !m.indices.isEmpty else { continue }
            var d = MeshDescriptor(name: "clash")
            d.positions = MeshBuffers.Positions((0..<(m.positions.count / 3)).map {
                SIMD3(m.positions[3 * $0], m.positions[3 * $0 + 1], m.positions[3 * $0 + 2])
            })
            d.primitives = .triangles(m.indices)
            if let mesh = try? MeshResource.generate(from: [d]) {
                root.addChild(ModelEntity(mesh: mesh, materials: [tint(.systemRed, 0.6)]))
            }
        }
    }
    parent.addChild(root)
}
