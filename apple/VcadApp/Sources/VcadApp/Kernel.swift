import RealityKit
#if canImport(AppKit)
import AppKit
#endif
import simd
import Metal
import CVcadFFI

/// CPU-side mesh data copied out of a kernel `VcadMeshView`, with normals
/// synthesized when the kernel didn't provide a matching-length normal buffer
/// (grounding risk #4 — the normals==vertices invariant is debug-only).
struct KernelMesh {
    var positions: [SIMD3<Float>]
    var normals: [SIMD3<Float>]
    var indices: [UInt32]
    var minBound: SIMD3<Float>
    var maxBound: SIMD3<Float>

    var triangleCount: Int { indices.count / 3 }
    var isEmpty: Bool { positions.isEmpty || indices.count < 3 }

    static func fromView(_ v: VcadMeshView) -> KernelMesh {
        let vertexCount = v.vertices_len / 3

        var positions = [SIMD3<Float>](repeating: .zero, count: vertexCount)
        if let vp = v.vertices {
            for i in 0..<vertexCount {
                positions[i] = SIMD3<Float>(vp[i * 3], vp[i * 3 + 1], vp[i * 3 + 2])
            }
        }

        var indices = [UInt32](repeating: 0, count: v.indices_len)
        if let ip = v.indices {
            for i in 0..<v.indices_len { indices[i] = ip[i] }
        }

        // Kernel normals are exact for BRep tessellations (analytic surface
        // normals, per-face vertices) — keep them. But STL imports carry
        // "last face wins" per-vertex normals: a hole-rim vertex takes the
        // countersink's normal and flat-face slivers shade into wedges. Those
        // show up as corners pointing far off their own face, so when enough
        // do, derive crease-split normals from the geometry instead.
        var normals: [SIMD3<Float>] = []
        if v.normals_len == v.vertices_len, v.normals_len > 0, let np = v.normals {
            normals = (0..<vertexCount).map { SIMD3<Float>(np[$0 * 3], np[$0 * 3 + 1], np[$0 * 3 + 2]) }
        }
        if normals.isEmpty || !Self.normalsTrustworthy(normals, positions: positions, indices: indices) {
            (positions, normals, indices) = Self.creasedNormals(positions: positions, indices: indices)
        }

        var lo = SIMD3<Float>(repeating: .greatestFiniteMagnitude)
        var hi = SIMD3<Float>(repeating: -.greatestFiniteMagnitude)
        for p in positions { lo = simd_min(lo, p); hi = simd_max(hi, p) }
        if vertexCount == 0 { lo = .zero; hi = .zero }

        return KernelMesh(positions: positions, normals: normals, indices: indices,
                          minBound: lo, maxBound: hi)
    }

    /// False when more than 0.5% of triangle corners carry a normal more
    /// than 60° off their own face — impossible for real smooth/analytic
    /// normals, typical of per-vertex "last face wins" STL normals.
    static func normalsTrustworthy(_ normals: [SIMD3<Float>], positions: [SIMD3<Float>],
                                   indices: [UInt32]) -> Bool {
        let triCount = indices.count / 3
        guard triCount > 0 else { return true }
        var bad = 0, seen = 0
        for t in 0..<triCount {
            let a = Int(indices[3 * t]), b = Int(indices[3 * t + 1]), c = Int(indices[3 * t + 2])
            guard a < positions.count, b < positions.count, c < positions.count,
                  a < normals.count, b < normals.count, c < normals.count else { continue }
            let f = cross(positions[b] - positions[a], positions[c] - positions[a])
            let l = length(f)
            guard l > 1e-12 else { continue }
            let fn = f / l
            for v in [a, b, c] {
                seen += 1
                if dot(fn, normals[v]) < 0.5 { bad += 1 }
            }
        }
        return seen == 0 || Double(bad) / Double(seen) <= 0.005
    }

    /// Crease angle for synthesized normals: faces meeting more sharply than
    /// this keep their own normal (a hard edge); gentler joins blend smooth.
    static let creaseAngleDegrees: Float = 30

    /// Normals for a mesh that came without any (imported STL/OBJ), split at
    /// a crease angle the way Fusion/Blender shade meshes. A corner shared by
    /// faces at 90° gets one vertex copy per side instead of one averaged
    /// normal, so flat faces read flat and curved ones stay smooth. Returns
    /// the (possibly grown) vertex arrays alongside the normals.
    static func creasedNormals(positions: [SIMD3<Float>], indices rawIndices: [UInt32])
        -> ([SIMD3<Float>], [SIMD3<Float>], [UInt32]) {
        let triCount = rawIndices.count / 3
        guard triCount > 0 else {
            return (positions, [SIMD3<Float>](repeating: [0, 0, 1], count: positions.count), rawIndices)
        }
        // Weld by position (1 µm grid) so adjacency is seen even when the
        // source emits unshared vertices (BRep tessellations do): otherwise a
        // smooth cylinder would shade as flat facets.
        var canonOf: [SIMD3<Int32>: UInt32] = [:]
        canonOf.reserveCapacity(positions.count)
        let canon: [UInt32] = positions.enumerated().map { i, p in
            let key = SIMD3<Int32>(Int32(clamping: Int((p.x * 1000).rounded())),
                                   Int32(clamping: Int((p.y * 1000).rounded())),
                                   Int32(clamping: Int((p.z * 1000).rounded())))
            if let c = canonOf[key] { return c }
            canonOf[key] = UInt32(i)
            return UInt32(i)
        }
        let indices = rawIndices.map { Int($0) < canon.count ? canon[Int($0)] : $0 }
        // Area-weighted face normal per triangle (zero for degenerate).
        var faceN = [SIMD3<Float>](repeating: .zero, count: triCount)
        var incident = [[Int]](repeating: [], count: positions.count)
        for t in 0..<triCount {
            let a = Int(indices[3 * t]), b = Int(indices[3 * t + 1]), c = Int(indices[3 * t + 2])
            guard a < positions.count, b < positions.count, c < positions.count else { continue }
            faceN[t] = cross(positions[b] - positions[a], positions[c] - positions[a])
            incident[a].append(t); incident[b].append(t); incident[c].append(t)
        }
        let cosCrease = cos(creaseAngleDegrees * .pi / 180)
        func unit(_ v: SIMD3<Float>) -> SIMD3<Float>? {
            let l = length(v); return l > 1e-12 ? v / l : nil
        }
        // Planar regions: triangles joined edge-to-edge with (near) identical
        // normals. A region of 3+ triangles (or a squarish 2-triangle quad) is a real flat face and shades
        // with its exact plane normal — no blending — so the long sliver
        // triangles CAD exporters fan across flat faces can't smear a tilted
        // rim/hole normal into wedge-shaped "ears". (Two is not enough: every
        // facet of a tessellated cylinder is a coplanar quad = 2 triangles,
        // and those must keep blending smooth.)
        let unitN = faceN.map { unit($0) }
        var parent = Array(0..<triCount)
        func find(_ x: Int) -> Int {
            var x = x
            while parent[x] != x { parent[x] = parent[parent[x]]; x = parent[x] }
            return x
        }
        var edgeFace: [UInt64: Int] = [:]
        for t in 0..<triCount {
            guard let nt = unitN[t] else { continue }
            for k in 0..<3 {
                let a = indices[3 * t + k], b = indices[3 * t + (k + 1) % 3]
                let key = UInt64(min(a, b)) << 32 | UInt64(max(a, b))
                if let o = edgeFace[key] {
                    if let no = unitN[o], dot(no, nt) > 0.9995 {   // ~1.8°: sliver normals are noisy
                        let ra = find(t), rb = find(o)
                        if ra != rb { parent[ra] = rb }
                    }
                } else {
                    edgeFace[key] = t
                }
            }
        }
        var regionSize = [Int](repeating: 0, count: triCount)
        var regionSum = [SIMD3<Float>](repeating: .zero, count: triCount)
        var regionTris = [[Int]](repeating: [], count: triCount)
        for t in 0..<triCount {
            let r = find(t)
            regionSize[r] += 1
            regionSum[r] += faceN[t]           // area-weighted: slivers barely count
            if regionTris[r].count < 3 { regionTris[r].append(t) }
        }
        // A 2-triangle region is a flat face when it is roughly square (a
        // prong side); a long thin quad is a strip of a curved tessellation
        // and keeps blending.
        func isFlatFace(_ r: Int) -> Bool {
            if regionSize[r] >= 3 { return true }
            guard regionSize[r] == 2 else { return false }
            var count: [UInt64: Int] = [:]
            var len: [UInt64: Float] = [:]
            for t in regionTris[r] {
                for k in 0..<3 {
                    let a = indices[3 * t + k], b = indices[3 * t + (k + 1) % 3]
                    let key = UInt64(min(a, b)) << 32 | UInt64(max(a, b))
                    count[key, default: 0] += 1
                    len[key] = distance(positions[Int(a)], positions[Int(b)])
                }
            }
            let outer = count.filter { $0.value == 1 }.compactMap { len[$0.key] }
            guard let lo = outer.min(), let hi = outer.max(), lo > 0 else { return false }
            return hi / lo <= 4
        }
        var flat = [Bool](repeating: false, count: triCount)
        for t in 0..<triCount where find(t) == t { flat[t] = isFlatFace(t) }
        var outPos: [SIMD3<Float>] = [], outN: [SIMD3<Float>] = []
        var outIdx = [UInt32](repeating: 0, count: triCount * 3)
        // Per source vertex: normals already emitted, so corners that land on
        // the same smoothing group share one output vertex.
        var emitted = [[(SIMD3<Float>, UInt32)]](repeating: [], count: positions.count)
        for t in 0..<triCount {
            let own = unitN[t]
            let region = find(t)
            let planar = own != nil && flat[region]
            for k in 0..<3 {
                let v = Int(indices[3 * t + k])
                guard v < positions.count else { outIdx[3 * t + k] = 0; continue }
                var sum = SIMD3<Float>.zero
                if planar {
                    sum = regionSum[region]
                } else if let own {
                    for f in incident[v] {
                        if let fn = unit(faceN[f]), dot(fn, own) >= cosCrease { sum += faceN[f] }
                    }
                }
                let n = unit(sum) ?? own ?? SIMD3<Float>(0, 0, 1)
                if let hit = emitted[v].first(where: { dot($0.0, n) > 0.9999 }) {
                    outIdx[3 * t + k] = hit.1
                } else {
                    let id = UInt32(outPos.count)
                    outPos.append(positions[v]); outN.append(n)
                    emitted[v].append((n, id))
                    outIdx[3 * t + k] = id
                }
            }
        }
        return (outPos, outN, outIdx)
    }

    func resource(name: String) -> MeshResource {
        var d = MeshDescriptor(name: name)
        d.positions = MeshBuffers.Positions(positions)
        d.normals = MeshBuffers.Normals(normals)
        d.primitives = .triangles(indices)
        return (try? MeshResource.generate(from: [d])) ?? .generateBox(size: 0.1)
    }
}

/// A GPU-resident mesh that streams kernel re-solves into its buffers without
/// reallocating — the M2 hot loop. Interleaves position+normal into one buffer
/// (stride 24: pos@0, normal@12) so RealityKit binds a single layout. Grows
/// (recreates) only when the re-solved mesh exceeds capacity, which bounds the
/// per-write work and prevents buffer overflow (grounding risk #3).
@MainActor
final class StreamingMesh {
    private var mesh: LowLevelMesh?
    private var vertexCapacity = 0
    private var indexCapacity = 0
    private(set) var resource: MeshResource?

    /// Stream a kernel mesh into the GPU buffers. Returns true when the
    /// underlying mesh (and thus `resource`) was recreated for more capacity —
    /// the caller must then reassign `resource` onto its entity.
    @discardableResult
    func update(from km: KernelMesh) -> Bool {
        let vcount = km.positions.count
        let icount = km.indices.count
        guard vcount > 0, icount > 0 else { return false }

        var recreated = false
        if mesh == nil || vcount > vertexCapacity || icount > indexCapacity {
            vertexCapacity = vcount + vcount / 2 + 64        // 1.5x headroom
            indexCapacity = icount + icount / 2 + 192
            mesh = try? Self.makeMesh(vCap: vertexCapacity, iCap: indexCapacity)
            recreated = true
        }
        guard let mesh else { resource = nil; return true }

        // Interleaved [px,py,pz, nx,ny,nz] per vertex — raw floats, not SIMD3
        // (which pads to 16). Stride 24 bytes matches the layout.
        mesh.replaceUnsafeMutableBytes(bufferIndex: 0) { raw in
            let f = raw.bindMemory(to: Float.self)
            for i in 0..<vcount {
                let p = km.positions[i], n = km.normals[i]
                let o = i * 6
                f[o] = p.x; f[o + 1] = p.y; f[o + 2] = p.z
                f[o + 3] = n.x; f[o + 4] = n.y; f[o + 5] = n.z
            }
        }
        mesh.replaceUnsafeMutableIndices { raw in
            let idx = raw.bindMemory(to: UInt32.self)
            for i in 0..<icount { idx[i] = km.indices[i] }
        }
        mesh.parts.replaceAll([
            LowLevelMesh.Part(indexCount: icount, topology: .triangle,
                              bounds: BoundingBox(min: km.minBound, max: km.maxBound))
        ])

        if recreated || resource == nil {
            resource = try? MeshResource(from: mesh)
        }
        return recreated
    }

    /// Bounds + counts computed while streaming, so callers don't need a
    /// CPU-side copy of the mesh just to know its extent.
    struct StreamStats {
        var minBound: SIMD3<Float>
        var maxBound: SIMD3<Float>
        var triangleCount: Int
        var vertexCount: Int
    }

    /// Direct FFI path: write the kernel's tessellation buffers straight into
    /// the GPU vertex/index buffers — no intermediate Swift arrays. Normals are
    /// synthesized in-buffer (area-weighted) when the kernel view doesn't carry
    /// a matching-length normal buffer. Falls back to the MeshDescriptor path
    /// (via KernelMesh) if LowLevelMesh creation fails.
    func update(fromView v: VcadMeshView) -> (recreated: Bool, stats: StreamStats)? {
        let vcount = v.vertices_len / 3
        let icount = v.indices_len
        guard vcount > 0, icount >= 3, let vp = v.vertices, let ip = v.indices else { return nil }

        var recreated = false
        if mesh == nil || vcount > vertexCapacity || icount > indexCapacity {
            vertexCapacity = vcount + vcount / 2 + 64        // 1.5x headroom
            indexCapacity = icount + icount / 2 + 192
            mesh = try? Self.makeMesh(vCap: vertexCapacity, iCap: indexCapacity)
            recreated = true
        }
        guard let mesh else {
            // MeshDescriptor fallback — still renders, just not streamable.
            let km = KernelMesh.fromView(v)
            resource = km.resource(name: "fallback")
            return (true, StreamStats(minBound: km.minBound, maxBound: km.maxBound,
                                      triangleCount: km.triangleCount,
                                      vertexCount: km.positions.count))
        }

        var lo = SIMD3<Float>(repeating: .greatestFiniteMagnitude)
        var hi = SIMD3<Float>(repeating: -.greatestFiniteMagnitude)
        let hasNormals = v.normals_len == v.vertices_len && v.normals != nil
        let np = v.normals

        mesh.replaceUnsafeMutableBytes(bufferIndex: 0) { raw in
            let f = raw.bindMemory(to: Float.self)
            for i in 0..<vcount {
                let s = i * 3, o = i * 6
                let x = vp[s], y = vp[s + 1], z = vp[s + 2]
                f[o] = x; f[o + 1] = y; f[o + 2] = z
                lo = simd_min(lo, SIMD3<Float>(x, y, z))
                hi = simd_max(hi, SIMD3<Float>(x, y, z))
                if hasNormals, let np {
                    f[o + 3] = np[s]; f[o + 4] = np[s + 1]; f[o + 5] = np[s + 2]
                } else {
                    f[o + 3] = 0; f[o + 4] = 0; f[o + 5] = 0
                }
            }
            if !hasNormals {
                // Area-weighted normals synthesized in the GPU buffer itself:
                // accumulate face normals into the normal slots, then normalize.
                var t = 0
                while t + 2 < icount {
                    let a = Int(ip[t]), b = Int(ip[t + 1]), c = Int(ip[t + 2])
                    t += 3
                    guard a < vcount, b < vcount, c < vcount else { continue }
                    let pa = SIMD3<Float>(f[a * 6], f[a * 6 + 1], f[a * 6 + 2])
                    let pb = SIMD3<Float>(f[b * 6], f[b * 6 + 1], f[b * 6 + 2])
                    let pc = SIMD3<Float>(f[c * 6], f[c * 6 + 1], f[c * 6 + 2])
                    let fn = cross(pb - pa, pc - pa)
                    for j in [a, b, c] {
                        f[j * 6 + 3] += fn.x; f[j * 6 + 4] += fn.y; f[j * 6 + 5] += fn.z
                    }
                }
                for i in 0..<vcount {
                    let o = i * 6
                    var n = SIMD3<Float>(f[o + 3], f[o + 4], f[o + 5])
                    let len = length(n)
                    n = len > 1e-6 ? n / len : SIMD3<Float>(0, 0, 1)
                    f[o + 3] = n.x; f[o + 4] = n.y; f[o + 5] = n.z
                }
            }
        }
        mesh.replaceUnsafeMutableIndices { raw in
            let idx = raw.bindMemory(to: UInt32.self)
            for i in 0..<icount { idx[i] = ip[i] }
        }
        mesh.parts.replaceAll([
            LowLevelMesh.Part(indexCount: icount, topology: .triangle,
                              bounds: BoundingBox(min: lo, max: hi))
        ])

        if recreated || resource == nil {
            resource = try? MeshResource(from: mesh)
        }
        return (recreated, StreamStats(minBound: lo, maxBound: hi,
                                       triangleCount: icount / 3, vertexCount: vcount))
    }

    private static func makeMesh(vCap: Int, iCap: Int) throws -> LowLevelMesh {
        let desc = LowLevelMesh.Descriptor(
            vertexCapacity: vCap,
            vertexAttributes: [
                LowLevelMesh.Attribute(semantic: .position, format: .float3, layoutIndex: 0, offset: 0),
                LowLevelMesh.Attribute(semantic: .normal, format: .float3, layoutIndex: 0, offset: 12),
            ],
            vertexLayouts: [LowLevelMesh.Layout(bufferIndex: 0, bufferOffset: 0, bufferStride: 24)],
            indexCapacity: iCap,
            indexType: .uint32
        )
        return try LowLevelMesh(descriptor: desc)
    }
}

/// A renderable, auto-fit scene: meshes + their colors, plus the combined
/// bounds so the viewport can center and scale arbitrary part sizes.
/// `edges[i]` (when present) holds part i's feature-edge segments as flat
/// endpoint pairs, for the CAD edge overlay.
struct RenderScene {
    var meshes: [(mesh: MeshResource, color: NSColor)]
    var center: SIMD3<Float>
    var size: Float
    var triangleCount: Int
    var partCount: Int
    var edges: [[SIMD3<Float>]] = []
    /// Assembly instances (rendered INSTEAD of `meshes` when non-empty,
    /// mirroring the web viewport). Each carries its part-def-local mesh and
    /// its kernel-frame world transform, so playback can re-pose the entity
    /// per frame without touching the mesh.
    var instances: [RenderInstance] = []
    /// Index-aligned with `meshes`: non-nil when part i is a pattern rendered
    /// as one shared MeshResource + N per-instance transforms (else one entity).
    var instancing: [PatternInstancing?] = []
    /// The kernel is still evaluating this document on a background thread;
    /// the viewport keeps whatever it was showing until the real scene lands.
    var solving = false

    static let empty = RenderScene(meshes: [], center: .zero, size: 100, triangleCount: 0, partCount: 0)
    static var pending: RenderScene { var s = RenderScene.empty; s.solving = true; return s }
}

/// One assembly instance ready to render. `index` is the FFI instance index —
/// entity names ("inst<i>") and playback transform order both key off it.
struct RenderInstance {
    var index: Int
    var id: String
    var mesh: MeshResource
    var material: ResolvedMaterial
    /// World placement in kernel coordinates (Z-up, mm), column-major.
    var transform: float4x4
    /// Model-edge ink lines in the mesh's local frame (screen-line layout),
    /// shared by every instance of one part definition. Nil = none.
    var edges: MeshResource? = nil
}

/// Feature-edge overlay support: converts a kernel `VcadEdgesView` into
/// segment endpoints and builds a renderable ribbon mesh (RealityKit has no
/// line primitive, so each segment becomes two crossed quads — reads as a
/// crisp line from every direction at CAD zoom levels).
enum EdgeOverlay {
    /// Flat endpoint pairs [a0, b0, a1, b1, ...] from the FFI view.
    static func segments(fromView v: VcadEdgesView) -> [SIMD3<Float>] {
        guard let p = v.floats, v.floats_len >= 6 else { return [] }
        let segCount = v.floats_len / 6
        var out = [SIMD3<Float>]()
        out.reserveCapacity(segCount * 2)
        for s in 0..<segCount {
            let o = s * 6
            out.append(SIMD3<Float>(p[o], p[o + 1], p[o + 2]))
            out.append(SIMD3<Float>(p[o + 3], p[o + 4], p[o + 5]))
        }
        return out
    }

    /// Build one mesh holding every segment as two crossed quads of the given
    /// world-space width. Returns nil for empty input.
    /// One quad per segment for the screen-space edge shader
    /// (`Shaders/EdgeLines.metal`): both endpoints twice, the segment
    /// direction in the normal and the side (-1/+1) in uv.x. The shader faces
    /// it to the camera at a constant pixel width.
    static func screenLineResource(segments: [SIMD3<Float>], name: String) -> MeshResource? {
        let segCount = segments.count / 2
        guard segCount > 0 else { return nil }
        var pos = [SIMD3<Float>](), nrm = [SIMD3<Float>](), uv = [SIMD2<Float>](), idx = [UInt32]()
        pos.reserveCapacity(segCount * 4); nrm.reserveCapacity(segCount * 4)
        uv.reserveCapacity(segCount * 4); idx.reserveCapacity(segCount * 12)
        for s in 0..<segCount {
            let a = segments[s * 2], b = segments[s * 2 + 1]
            let d = b - a
            let len = length(d)
            guard len > 1e-6 else { continue }
            let dir = d / len
            let o = UInt32(pos.count)
            pos += [a, a, b, b]
            nrm += [dir, dir, dir, dir]
            uv += [[-1, 0], [1, 0], [1, 1], [-1, 1]]
            idx += [o, o + 1, o + 2, o, o + 2, o + 3, o, o + 2, o + 1, o, o + 3, o + 2]
        }
        guard !pos.isEmpty else { return nil }
        var d = MeshDescriptor(name: name)
        d.positions = MeshBuffers.Positions(pos)
        d.normals = MeshBuffers.Normals(nrm)
        d.textureCoordinates = MeshBuffers.TextureCoordinates(uv)
        d.primitives = .triangles(idx)
        return try? MeshResource.generate(from: [d])
    }

    static func ribbonResource(segments: [SIMD3<Float>], width: Float, name: String) -> MeshResource? {
        let segCount = segments.count / 2
        guard segCount > 0, width > 0 else { return nil }
        var positions = [SIMD3<Float>]()
        var indices = [UInt32]()
        positions.reserveCapacity(segCount * 8)
        indices.reserveCapacity(segCount * 24)
        let h = width / 2
        for s in 0..<segCount {
            let a = segments[s * 2], b = segments[s * 2 + 1]
            let d = b - a
            let len = length(d)
            guard len > 1e-6 else { continue }
            let dir = d / len
            // Two perpendicular in-plane axes for the crossed quads.
            let ref: SIMD3<Float> = abs(dir.z) < 0.9 ? [0, 0, 1] : [1, 0, 0]
            let u = normalize(cross(dir, ref))
            let v = normalize(cross(dir, u))
            let base = UInt32(positions.count)
            positions.append(contentsOf: [
                a - u * h, a + u * h, b + u * h, b - u * h,
                a - v * h, a + v * h, b + v * h, b - v * h,
            ])
            for q in 0..<2 {
                let o = base + UInt32(q * 4)
                // Both windings so the quad is visible from either side
                // without needing a double-sided material.
                indices.append(contentsOf: [o, o + 1, o + 2, o, o + 2, o + 3])
                indices.append(contentsOf: [o, o + 2, o + 1, o, o + 3, o + 2])
            }
        }
        guard !positions.isEmpty else { return nil }
        var d = MeshDescriptor(name: name)
        d.positions = MeshBuffers.Positions(positions)
        // Unlit material ignores shading, but the generator wants normals.
        d.normals = MeshBuffers.Normals([SIMD3<Float>](repeating: [0, 0, 1], count: positions.count))
        d.primitives = .triangles(indices)
        return try? MeshResource.generate(from: [d])
    }
}
