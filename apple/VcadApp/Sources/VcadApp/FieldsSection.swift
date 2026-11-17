import SwiftUI
import RealityKit
import AppKit

// The particle-optics section: potential heatmap, equipotentials, wire cross
// sections and deuteron tracks, drawn once with CoreGraphics and shown two
// ways — as an image in the Fields panel, and as a textured quad on the
// device's own section plane in the viewport (same quad-in-the-kernel-frame
// approach as the compressed-field slice in FieldOverlay.swift).
//
// Frame (from the solver): kernel mm, device axis = world Z through the
// origin, section plane y = 0, x = signed r. A device modeled about the
// origin on Z sits exactly on it; the HUD says so.

/// Colors chosen for camera legibility on a dark viewport.
enum FieldsPalette {
    /// Deep well (cathode) → wall: indigo → teal → pale gold.
    static func potential(_ t: Double) -> (Double, Double, Double) {
        let stops: [(Double, (Double, Double, Double))] = [
            (0.0, (0.98, 0.93, 0.62)), (0.25, (0.94, 0.62, 0.30)), (0.5, (0.62, 0.30, 0.48)),
            (0.75, (0.25, 0.20, 0.50)), (1.0, (0.06, 0.08, 0.20)),
        ]
        let x = min(1, max(0, t))
        for k in 1..<stops.count where x <= stops[k].0 {
            let (a, ca) = stops[k - 1], (b, cb) = stops[k]
            let u = (x - a) / (b - a)
            return (ca.0 + (cb.0 - ca.0) * u, ca.1 + (cb.1 - ca.1) * u, ca.2 + (cb.2 - ca.2) * u)
        }
        return stops.last!.1
    }
    static let survived = NSColor(red: 0.35, green: 0.90, blue: 1.00, alpha: 0.95)
    static let wire = NSColor(red: 1.00, green: 0.55, blue: 0.20, alpha: 0.95)
    static let wall = NSColor(red: 0.95, green: 0.40, blue: 0.75, alpha: 0.95)
    static func track(_ fate: String) -> NSColor { fate == "wire" ? wire : fate == "wall" ? wall : survived }
    static func trackColor(_ fate: String) -> Color { Color(nsColor: track(fate)) }
}

struct SectionStyle: Equatable {
    var heatmap = true
    var tracks = true
    var contours = true
    /// Heatmap opacity (the viewport quad wants it translucent).
    var heatAlpha = 1.0
}

/// Render the full (mirrored) section to an image `px` pixels wide.
func renderSection(_ s: ParticleRun.Section, px: Int, style: SectionStyle) -> CGImage? {
    let R = s.r_max_mm, H = s.z_half_mm
    let w = px, h = max(8, Int((Double(px) * H / R).rounded()))
    guard let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                              space: CGColorSpace(name: CGColorSpace.sRGB)!,
                              bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
    let sx = Double(w) / (2 * R), sy = Double(h) / (2 * H)
    func P(_ x: Double, _ z: Double) -> CGPoint { CGPoint(x: (x + R) * sx, y: (z + H) * sy) }
    let lineScale = Double(px) / 1000

    if style.heatmap {
        // Bilinear sample of the r ≥ 0 grid at every pixel, mirrored.
        let g = s.potential
        let span = g.max_v - g.min_v
        var rgba = [UInt8](repeating: 0, count: w * h * 4)
        for py in 0..<h {
            let z = (Double(py) + 0.5) / sy - H
            let fj = min(Double(g.nz - 1), max(0, (z + H) / g.dz_mm))
            let j0 = min(g.nz - 2, Int(fj)), fw = fj - Double(j0)
            for pxi in 0..<w {
                let r = abs((Double(pxi) + 0.5) / sx - R)
                let fi = min(Double(g.nr - 1), r / g.dr_mm)
                let i0 = min(g.nr - 2, Int(fi)), fu = fi - Double(i0)
                let v00 = g.values[i0 * g.nz + j0], v10 = g.values[(i0 + 1) * g.nz + j0]
                let v01 = g.values[i0 * g.nz + j0 + 1], v11 = g.values[(i0 + 1) * g.nz + j0 + 1]
                let v = v00 * (1 - fu) * (1 - fw) + v10 * fu * (1 - fw) + v01 * (1 - fu) * fw + v11 * fu * fw
                // t = 0 at the deepest point, 1 at the wall.
                let t = span > 0 ? (v - g.min_v) / span : 1
                let (cr, cg, cb) = FieldsPalette.potential(t)
                let a = style.heatAlpha
                // CG bitmap rows run bottom-up in this context's memory? No: row 0 is the TOP.
                let o = ((h - 1 - py) * w + pxi) * 4
                rgba[o] = UInt8(255 * cr * a); rgba[o + 1] = UInt8(255 * cg * a)
                rgba[o + 2] = UInt8(255 * cb * a); rgba[o + 3] = UInt8(255 * a)
            }
        }
        if let provider = CGDataProvider(data: Data(rgba) as CFData),
           let img = CGImage(width: w, height: h, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: w * 4,
                             space: CGColorSpace(name: CGColorSpace.sRGB)!,
                             bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
                             provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent) {
            ctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h))
        }
    }

    ctx.setLineCap(.round); ctx.setLineJoin(.round)
    if style.contours {
        ctx.setStrokeColor(NSColor.white.withAlphaComponent(0.55).cgColor)
        ctx.setLineWidth(1.6 * lineScale)
        for level in s.equipotentials {
            for line in level.polylines where line.count > 1 {
                for mirror in [1.0, -1.0] {
                    ctx.beginPath()
                    ctx.move(to: P(mirror * line[0][0], line[0][1]))
                    for p in line.dropFirst() { ctx.addLine(to: P(mirror * p[0], p[1])) }
                    ctx.strokePath()
                }
            }
        }
    }
    if style.tracks {
        ctx.setLineWidth(2.4 * lineScale)
        for t in s.tracks where t.points.count > 1 {
            ctx.setStrokeColor(FieldsPalette.track(t.fate).withAlphaComponent(0.8).cgColor)
            ctx.beginPath()
            ctx.move(to: P(t.points[0][0], t.points[0][1]))
            for p in t.points.dropFirst() { ctx.addLine(to: P(p[0], p[1])) }
            ctx.strokePath()
        }
    }
    // Wires: filled cross sections both sides of the axis.
    for wire in s.wires {
        for mirror in [1.0, -1.0] {
            let c = P(mirror * wire.r_mm, wire.z_mm)
            let rr = max(3 * lineScale, wire.radius_mm * sx)
            let rect = CGRect(x: c.x - rr, y: c.y - rr, width: 2 * rr, height: 2 * rr)
            ctx.setFillColor(NSColor(white: 0.88, alpha: 1).cgColor)
            ctx.fillEllipse(in: rect)
            ctx.setStrokeColor(NSColor.black.withAlphaComponent(0.7).cgColor)
            ctx.setLineWidth(1.2 * lineScale)
            ctx.strokeEllipse(in: rect)
        }
    }
    // Chamber wall outline.
    ctx.setStrokeColor(NSColor(white: 0.85, alpha: 0.9).cgColor)
    ctx.setLineWidth(3 * lineScale)
    ctx.stroke(CGRect(x: 1, y: 1, width: w - 2, height: h - 2))
    return ctx.makeImage()
}

// MARK: - Viewport

/// Build or update the section quad under `parent` (the kernel-mm frame).
@MainActor
func syncFieldsSection(_ store: FieldsStore, in parent: Entity) {
    let analysis = AnalysisStore.shared
    let visible = analysis.shown && analysis.tab == .fields && store.showSection && store.particle?.section != nil
    let key = "fieldsSection-\(store.overlayRevision)-\(store.showTracks)-\(store.showHeatmap)"
    if let existing = parent.findEntity(named: key) { existing.isEnabled = visible; return }
    parent.children.filter { $0.name.hasPrefix("fieldsSection") }.forEach { $0.removeFromParent() }
    guard visible, let s = store.particle?.section else { return }
    let style = SectionStyle(heatmap: store.showHeatmap, tracks: store.showTracks, contours: true, heatAlpha: 0.82)
    guard let image = renderSection(s, px: 1600, style: style),
          let texture = try? TextureResource(image: image, options: .init(semantic: .color)) else { return }
    let R = Float(s.r_max_mm), H = Float(s.z_half_mm)
    var d = MeshDescriptor(name: "fieldsSection")
    // y = 0 plane, image row 0 (top) at z = +H → v = 1 at corners 2, 3.
    d.positions = MeshBuffers.Positions([[-R, 0, -H], [R, 0, -H], [R, 0, H], [-R, 0, H]])
    d.textureCoordinates = MeshBuffers.TextureCoordinates([[0, 0], [1, 0], [1, 1], [0, 1]])
    d.primitives = .triangles([0, 1, 2, 0, 2, 3])
    var m = UnlitMaterial()
    m.color = .init(tint: .white, texture: .init(texture))
    m.blending = .transparent(opacity: 1.0)
    m.faceCulling = .none
    guard let mesh = try? MeshResource.generate(from: [d]) else { return }
    let e = ModelEntity(mesh: mesh, materials: [m])
    e.name = key
    parent.addChild(e)
}

// MARK: - Panel view

/// The section image with a frame-of-reference caption and fate legend.
struct SectionImageView: View {
    let section: ParticleRun.Section
    var style = SectionStyle()

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.xs) {
            if let img = renderSection(section, px: 900, style: style) {
                Image(decorative: img, scale: 1).resizable()
                    .aspectRatio(section.r_max_mm / section.z_half_mm, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.control, style: .continuous))
                    .overlay(alignment: .bottomLeading) {
                        Text(String(format: "%.0f mm", 2 * section.r_max_mm))
                            .font(.caption.monospacedDigit()).foregroundStyle(.white.opacity(0.85))
                            .padding(6)
                    }
                    .accessibilityLabel("Equipotentials and deuteron tracks in the device section")
            }
            HStack(spacing: Theme.Space.m) {
                legend("recirculating", "survived")
                legend("hit grid", "wire")
                legend("hit wall", "wall")
                Spacer(minLength: 0)
                HStack(spacing: 4) {
                    LinearGradient(colors: stride(from: 0.0, through: 1.0, by: 0.125).map { t in
                        let (r, g, b) = FieldsPalette.potential(t)
                        return Color(red: r, green: g, blue: b)
                    }, startPoint: .leading, endPoint: .trailing)
                    .frame(width: 54, height: 8).clipShape(Capsule())
                    Text(String(format: "%.0f…0 kV", section.potential.min_v / 1000))
                        .font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                }
            }
        }
    }

    private func legend(_ label: String, _ fate: String) -> some View {
        HStack(spacing: 4) {
            Capsule().fill(FieldsPalette.trackColor(fate)).frame(width: 14, height: 3)
            Text(label).font(.caption).foregroundStyle(.secondary)
        }
    }
}
