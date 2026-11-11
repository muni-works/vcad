import Foundation
import simd
import CoreGraphics
import CoreImage

/// The grounded contact shadow under the part: a baked, top-down occlusion
/// map of the part's real footprint, laid on the floor plane.
///
/// Two layers share one alpha texture:
/// - **contact** — only the triangles within a thin band above the floor,
///   lightly blurred: the tight darkening along the base edges that makes a
///   part read as *standing on* the grid rather than hovering over it;
/// - **ambient** — the whole top-down silhouette, heavily blurred and faint:
///   the soft pool under the part (and under overhangs).
///
/// Everything here is kernel millimetres, Z-up; the viewport maps the plane
/// into display space. Pure data in, pure data out — testable without
/// RealityKit.
struct ContactShadow {
    /// Where the shadow quad sits on the floor, kernel mm.
    struct Plane: Equatable {
        /// Centre of the quad in kernel XY (the footprint's bbox centre).
        var center: SIMD2<Float>
        /// Full side lengths in kernel X and Y, penumbra margin included.
        var size: SIMD2<Float>
        /// Floor height (the part's lowest Z).
        var floorZ: Float
        /// Penumbra margin added on every side, mm.
        var margin: Float
    }

    /// Texture side, px. 256 is plenty: the map is blurred anyway.
    static let pixels = 256
    /// Peak opacities of the two layers.
    static let contactOpacity: Float = 0.62
    static let ambientOpacity: Float = 0.30

    /// The quad for a part (or the whole scene) with bounds `lo…hi`. The
    /// margin grows with the footprint (a soft pool scales with the part) and
    /// with height (a tall part throws a wider ambient pool), with a floor
    /// so a sliver part still gets a visible penumbra.
    static func plane(lo: SIMD3<Float>, hi: SIMD3<Float>) -> Plane {
        let ext = simd_max(hi - lo, .zero)
        let foot = max(ext.x, ext.y)
        let margin = max(0.22 * foot + 0.12 * ext.z, 0.05 * max(foot, ext.z), 0.5)
        let c = (SIMD2(lo.x, lo.y) + SIMD2(hi.x, hi.y)) / 2
        let s = SIMD2(ext.x, ext.y) + 2 * margin
        return Plane(center: c, size: s, floorZ: lo.z, margin: margin)
    }

    /// Height of the "touching the floor" band, mm: geometry this close to
    /// the floor casts the tight contact layer.
    static func contactBand(lo: SIMD3<Float>, hi: SIMD3<Float>) -> Float {
        let ext = simd_max(hi - lo, .zero)
        return max(0.04 * simd_reduce_max(ext), 0.05)
    }

    /// Map kernel XY into texture pixel space (CoreGraphics, origin bottom-
    /// left): x → right, kernel +Y → up the image.
    static func pixel(_ p: SIMD2<Float>, plane: Plane) -> SIMD2<Float> {
        let n = Float(pixels)
        let uv = (p - plane.center) / plane.size + 0.5
        return uv * n
    }

    /// A triangle soup (kernel mm, 3 positions per triangle) split into the
    /// projected footprint and the near-floor contact subset.
    static func footprint(triangles: [SIMD3<Float>], floorZ: Float,
                          band: Float) -> (all: [SIMD2<Float>], contact: [SIMD2<Float>]) {
        var all: [SIMD2<Float>] = [], contact: [SIMD2<Float>] = []
        all.reserveCapacity(triangles.count)
        var i = 0
        while i + 2 < triangles.count {
            let a = triangles[i], b = triangles[i + 1], c = triangles[i + 2]
            i += 3
            let pa = SIMD2(a.x, a.y), pb = SIMD2(b.x, b.y), pc = SIMD2(c.x, c.y)
            // Edge-on triangles (walls seen from above) add nothing.
            let area2 = abs((pb.x - pa.x) * (pc.y - pa.y) - (pc.x - pa.x) * (pb.y - pa.y))
            if area2 < 1e-10 { continue }
            all += [pa, pb, pc]
            if min(a.z, b.z, c.z) <= floorZ + band { contact += [pa, pb, pc] }
        }
        return (all, contact)
    }

    /// A rectangle footprint as two triangles, for scenes with no triangle
    /// data (sandbox, assemblies).
    static func boxTriangles(lo: SIMD3<Float>, hi: SIMD3<Float>) -> [SIMD3<Float>] {
        let a = SIMD3(lo.x, lo.y, lo.z), b = SIMD3(hi.x, lo.y, lo.z)
        let c = SIMD3(hi.x, hi.y, lo.z), d = SIMD3(lo.x, hi.y, lo.z)
        return [a, b, c, a, c, d]
    }

    /// Bake the alpha map: 8-bit grey, 0 = clear, 255 = full shadow ink.
    /// Row 0 is the top of the image (kernel max Y).
    static func bake(triangles: [SIMD3<Float>], lo: SIMD3<Float>, hi: SIMD3<Float>) -> CGImage? {
        let plane = plane(lo: lo, hi: hi)
        let band = contactBand(lo: lo, hi: hi)
        // Cap the work on huge meshes; the map is blurred, so a strided
        // subset of a dense mesh draws the same silhouette.
        var tris = triangles
        let cap = 450_000
        if tris.count > cap {
            let stride = (tris.count / 3 + cap / 3 - 1) / (cap / 3)
            var s: [SIMD3<Float>] = []
            s.reserveCapacity(cap)
            var t = 0
            while t * 3 + 2 < tris.count { s += tris[(t * 3)..<(t * 3 + 3)]; t += stride }
            tris = s
        }
        let fp = footprint(triangles: tris, floorZ: plane.floorZ, band: band)
        let pxPerMM = Float(pixels) / max(plane.size.x, plane.size.y)
        guard let ambient = layer(fp.all, plane: plane, blurPx: max(2, plane.margin * 0.55 * pxPerMM)),
              let contact = layer(fp.contact, plane: plane, blurPx: max(1.2, plane.margin * 0.12 * pxPerMM))
        else { return nil }
        let n = pixels
        var out = [UInt8](repeating: 0, count: n * n)
        for k in 0..<(n * n) {
            let a = Float(ambient[k]) / 255 * ambientOpacity
            let c = Float(contact[k]) / 255 * contactOpacity
            // Screen-combine so the layers deepen together without clipping.
            let v = 1 - (1 - a) * (1 - c)
            out[k] = UInt8(max(0, min(255, (v * 255).rounded())))
        }
        return grey(out)
    }

    /// Fill the triangles, Gaussian-blur, read back as grey bytes (row 0 = top).
    private static func layer(_ pts: [SIMD2<Float>], plane: Plane, blurPx: Float) -> [UInt8]? {
        let n = pixels
        // Rasterize the triangles ourselves: one CGPath holding every
        // triangle made CoreGraphics resolve all the edge crossings between
        // overlapping triangles (~O(n²)) — a minute-long main-thread hang on
        // a 100k-triangle import. Plain edge-function coverage per triangle
        // costs its pixel area; the Gaussian blur below supplies the AA.
        var mask = [UInt8](repeating: 0, count: n * n)
        var i = 0
        while i + 2 < pts.count {
            let a = pixel(pts[i], plane: plane), b = pixel(pts[i + 1], plane: plane), c = pixel(pts[i + 2], plane: plane)
            i += 3
            let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
            if abs(area) < 1e-9 { continue }
            let s: Float = area > 0 ? 1 : -1
            let x0 = max(0, Int((min(a.x, b.x, c.x)).rounded(.down))), x1 = min(n - 1, Int((max(a.x, b.x, c.x)).rounded(.up)))
            let y0 = max(0, Int((min(a.y, b.y, c.y)).rounded(.down))), y1 = min(n - 1, Int((max(a.y, b.y, c.y)).rounded(.up)))
            if x0 > x1 || y0 > y1 { continue }
            for y in y0...y1 {
                let py = Float(y) + 0.5
                let row = (n - 1 - y) * n          // CG y-up → memory row 0 = top
                for x in x0...x1 {
                    let px = Float(x) + 0.5
                    let w0 = s * ((b.x - a.x) * (py - a.y) - (b.y - a.y) * (px - a.x))
                    let w1 = s * ((c.x - b.x) * (py - b.y) - (c.y - b.y) * (px - b.x))
                    let w2 = s * ((a.x - c.x) * (py - c.y) - (a.y - c.y) * (px - c.x))
                    if w0 >= 0, w1 >= 0, w2 >= 0 { mask[row + x] = 255 }
                }
            }
        }
        guard let img = grey(mask) else { return nil }
        let ci = CIImage(cgImage: img).clampedToExtent()
            .applyingGaussianBlur(sigma: Double(blurPx))
            .cropped(to: CGRect(x: 0, y: 0, width: n, height: n))
        var bytes = [UInt8](repeating: 0, count: n * n)
        let cictx = CIContext(options: [.workingColorSpace: NSNull(), .outputColorSpace: NSNull()])
        cictx.render(ci, toBitmap: &bytes, rowBytes: n, bounds: CGRect(x: 0, y: 0, width: n, height: n),
                     format: .L8, colorSpace: nil)
        return bytes
    }

    private static func grey(_ bytes: [UInt8]) -> CGImage? {
        let n = pixels
        guard let provider = CGDataProvider(data: Data(bytes) as CFData) else { return nil }
        return CGImage(width: n, height: n, bitsPerComponent: 8, bitsPerPixel: 8, bytesPerRow: n,
                       space: CGColorSpaceCreateDeviceGray(),
                       bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.none.rawValue),
                       provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent)
    }
}
