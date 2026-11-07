import SwiftUI
import RealityKit
import simd

// Section plane: a display-only cut through every part. The viewport swaps
// each part's mesh for a copy clipped against the plane (triangles on the far
// side dropped, crossing ones split) and draws the cut outline. The document
// is never touched.

struct SectionPlane: Equatable {
    var axis: OpAxis = .z
    /// Plane position along the axis, kernel mm.
    var offset: Double = 0
    /// Keep the positive side instead of the negative one.
    var flip = false

    var normal: SIMD3<Float> {
        let v = axis.vector
        return SIMD3(Float(v[0]), Float(v[1]), Float(v[2])) * (flip ? -1 : 1)
    }
    /// Signed distance; > 0 is the side that is cut away.
    func distance(_ p: SIMD3<Float>) -> Float {
        let v = axis.vector
        let along = p.x * Float(v[0]) + p.y * Float(v[1]) + p.z * Float(v[2]) - Float(offset)
        return flip ? -along : along
    }

    /// Clip a triangle mesh. Returns the kept triangles (flat, unindexed) and
    /// the cut outline as segment pairs.
    func clip(positions: [SIMD3<Float>], indices: [UInt32]) -> (tris: [SIMD3<Float>], outline: [SIMD3<Float>]) {
        var tris: [SIMD3<Float>] = []
        var outline: [SIMD3<Float>] = []
        tris.reserveCapacity(indices.count)
        for t in stride(from: 0, to: indices.count - 2, by: 3) {
            let p = [positions[Int(indices[t])], positions[Int(indices[t + 1])], positions[Int(indices[t + 2])]]
            let d = p.map(distance)
            let keep = d.map { $0 <= 0 }
            switch keep.filter({ $0 }).count {
            case 3: tris += p
            case 0: continue
            default:
                // Walk the triangle's edges, emitting the kept polygon.
                var poly: [SIMD3<Float>] = []
                var cut: [SIMD3<Float>] = []
                for i in 0..<3 {
                    let j = (i + 1) % 3
                    if keep[i] { poly.append(p[i]) }
                    if keep[i] != keep[j] {
                        let s = d[i] / (d[i] - d[j])
                        let x = p[i] + (p[j] - p[i]) * s
                        poly.append(x); cut.append(x)
                    }
                }
                for k in 1..<max(1, poly.count - 1) { tris += [poly[0], poly[k], poly[k + 1]] }
                if cut.count == 2 { outline += cut }
            }
        }
        return (tris, outline)
    }
}

extension SectionPlane {
    /// A mesh resource from flat clipped triangles (flat normals).
    static func resource(_ tris: [SIMD3<Float>], name: String) -> MeshResource? {
        guard tris.count >= 3 else { return nil }
        var normals: [SIMD3<Float>] = []
        normals.reserveCapacity(tris.count)
        for t in stride(from: 0, to: tris.count - 2, by: 3) {
            let n = simd_cross(tris[t + 1] - tris[t], tris[t + 2] - tris[t])
            let u = simd_length(n) > 1e-12 ? simd_normalize(n) : SIMD3<Float>(0, 0, 1)
            normals += [u, u, u]
        }
        var d = MeshDescriptor(name: name)
        d.positions = MeshBuffers.Positions(tris)
        d.normals = MeshBuffers.Normals(normals)
        d.primitives = .triangles((0..<UInt32(tris.count)).map { $0 })
        return try? MeshResource.generate(from: [d])
    }

    /// Positions + indices of every part of a RealityKit mesh.
    static func geometry(of mesh: MeshResource) -> (positions: [SIMD3<Float>], indices: [UInt32]) {
        var pos: [SIMD3<Float>] = []
        var idx: [UInt32] = []
        for model in mesh.contents.models {
            for part in model.parts {
                let base = UInt32(pos.count)
                pos += part.positions.elements
                if let tri = part.triangleIndices?.elements { idx += tri.map { $0 + base } }
            }
        }
        return (pos, idx)
    }
}

/// Section controls, floating while a section is on.
struct SectionPanel: View {
    @Bindable var model: EditorModel
    var body: some View {
        if let sec = model.section {
            let extent = Double(max(model.displayedSceneSize, 10))
            VStack(alignment: .leading, spacing: Theme.Space.s) {
                HStack {
                    Label("Section", systemImage: "square.split.diagonal").font(.headline)
                    Spacer()
                    Button { model.section = nil } label: { Image(systemName: "xmark") }
                        .buttonStyle(.borderless).help("Close section")
                }
                Picker("Axis", selection: Binding(get: { sec.axis }, set: { model.section?.axis = $0 })) {
                    ForEach(OpAxis.allCases) { Text($0.label).tag($0) }
                }.pickerStyle(.segmented).labelsHidden()
                Slider(value: Binding(get: { sec.offset }, set: { model.section?.offset = $0 }),
                       in: -extent...extent)
                HStack {
                    Text(String(format: "%.1f mm", sec.offset)).font(.caption.monospacedDigit())
                    Spacer()
                    Toggle("Flip", isOn: Binding(get: { sec.flip }, set: { model.section?.flip = $0 }))
                        .toggleStyle(.switch).controlSize(.mini)
                }
            }
            .padding(Theme.Space.m).frame(width: 240).panelSurface()
        }
    }
}
