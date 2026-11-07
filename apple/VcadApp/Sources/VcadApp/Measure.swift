import SwiftUI
import simd

// Measure tool: click points on faces/edges (the picking API records where),
// read lengths, areas, distances and angles. One pick describes the entity;
// two picks measure between them.

struct Measurement {
    var rows: [(label: String, value: String)]
    /// A line to draw between the two picked points, kernel mm.
    var span: (SIMD3<Float>, SIMD3<Float>)?
}

extension EditorModel {
    /// The last two picks while measuring.
    var measurePicks: [TopoRef] { Array(picking.selection.suffix(2)) }

    var measurement: Measurement? {
        let picks = measurePicks
        guard !picks.isEmpty else { return nil }
        var rows: [(String, String)] = []
        func mm(_ v: Float) -> String { String(format: "%.3f mm", v) }
        func pt(_ p: SIMD3<Float>) -> String { String(format: "%.2f, %.2f, %.2f", p.x, p.y, p.z) }

        func describe(_ r: TopoRef) -> (dir: SIMD3<Float>?, normal: SIMD3<Float>?, planarPoint: SIMD3<Float>?) {
            guard let topo = partTopology(r.part) else { return (nil, nil, nil) }
            switch r.kind {
            case .face:
                guard let f = topo.face(r.id) else { return (nil, nil, nil) }
                rows.append(("Face", "\(f.surface) · part \(r.part)"))
                rows.append(("Area", String(format: "%.3f mm²", f.area)))
                if f.isPlanar { rows.append(("Normal", pt(f.normalF))) }
                return (nil, f.isPlanar ? simd_normalize(f.normalF) : nil, f.isPlanar ? f.centerF : nil)
            case .edge:
                guard let e = topo.edge(r.id) else { return (nil, nil, nil) }
                rows.append(("Edge", "length \(mm(e.length))"))
                let p = e.segmentPoints
                let d = p.count >= 2 ? simd_normalize(p[p.count - 1] - p[0]) : nil
                return (d, nil, nil)
            }
        }

        let a = describe(picks[0])
        let ha = picking.hits[picks[0]]
        if let ha { rows.append(("Point", pt(ha.point))) }
        guard picks.count == 2 else { return Measurement(rows: rows, span: nil) }

        rows.append(("—", ""))
        let b = describe(picks[1])
        var span: (SIMD3<Float>, SIMD3<Float>)?
        if let ha, let hb = picking.hits[picks[1]] {
            let d = hb.point - ha.point
            rows.append(("Distance", mm(simd_length(d))))
            rows.append(("ΔX ΔY ΔZ", String(format: "%.2f, %.2f, %.2f", d.x, d.y, d.z)))
            span = (ha.point, hb.point)
        }
        // Parallel planar faces: the true face-to-face distance.
        if let na = a.normal, let nb = b.normal, let ca = a.planarPoint, let cb = b.planarPoint,
           abs(abs(simd_dot(na, nb)) - 1) < 1e-4 {
            rows.append(("Face gap", mm(abs(simd_dot(cb - ca, na)))))
        }
        if let va = a.normal ?? a.dir, let vb = b.normal ?? b.dir {
            let deg = acos(max(-1, min(1, abs(simd_dot(va, vb))))) * 180 / .pi
            rows.append(("Angle", String(format: "%.2f°", deg)))
        }
        return Measurement(rows: rows, span: span)
    }
}

/// Floating readout while the measure tool is on.
struct MeasurePanel: View {
    @Bindable var model: EditorModel
    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            HStack {
                Label("Measure", systemImage: "ruler").font(.headline)
                Spacer()
                Button { model.picking.clear() } label: { Image(systemName: "arrow.counterclockwise") }
                    .buttonStyle(.borderless).help("Clear picks")
                Button { model.measuring = false } label: { Image(systemName: "xmark") }
                    .buttonStyle(.borderless).help("Close measure")
            }
            if let m = model.measurement {
                ForEach(Array(m.rows.enumerated()), id: \.offset) { _, row in
                    if row.label == "—" { Divider() } else { KeyValueRow(row.label, row.value) }
                }
            } else {
                Text("Click a face, edge or point. Click a second one to measure between them.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(Theme.Space.m).frame(width: 260).panelSurface()
    }
}
