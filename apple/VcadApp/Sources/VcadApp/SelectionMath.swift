import Foundation
import CoreGraphics
import simd
import AppKit

// Pure helpers behind viewport selection: pixel-based pick tolerance, the
// window/crossing marquee rule, the selection readout, the shared selection
// colours, and the orbit-about-selection pivot. Kept free of RealityKit and
// view state so each rule is unit-testable on its own.

/// One colour scheme for every kind of selection (parts, faces, edges):
/// brand orange for the selection, a soft lighter tint of it for hover.
enum SelectionStyle {
    /// Brand orange.
    static let selected = NSColor(red: 1.0, green: 0.62, blue: 0.12, alpha: 1)
    /// Soft, lighter orange for what the pointer is over.
    static let hover = NSColor(red: 1.0, green: 0.80, blue: 0.55, alpha: 1)
    /// Hover on top of a selection: brighter than either, never "deselected".
    static let selectedHover = NSColor(red: 1.0, green: 0.70, blue: 0.28, alpha: 1)

    static let partSelectedIntensity: Float = 0.55
    static let partSelectedHoverIntensity: Float = 0.75
    static let partHoverIntensity: Float = 0.22

    static let faceFillSelectedAlpha: CGFloat = 0.48
    static let faceFillHoverAlpha: CGFloat = 0.28

    /// Screen-space widths (device px) of topology edge highlights.
    static let edgeSelectedPx: Float = 3.0
    static let edgeHoverPx: Float = 2.0
}

enum SelectionMath {
    /// The PerspectiveCamera's vertical field of view.
    static let fovDegrees: Float = 60

    /// World length (same units as `distance`) that `px` screen pixels cover at
    /// `distance` from a perspective camera with vertical FOV `fovDegrees`
    /// across a viewport `viewportHeight` pixels tall.
    static func pixelTolerance(px: Float, distance: Float, viewportHeight: Float,
                               fovDegrees: Float = fovDegrees) -> Float {
        guard viewportHeight > 0, distance > 0 else { return 0 }
        return px * 2 * tan(fovDegrees / 2 * .pi / 180) / viewportHeight * distance
    }

    /// Edge pick tolerance in mm: about 6 px at the given distance, floored so
    /// a degenerate distance still picks something.
    static func edgePickTolerance(distance: Float, viewportHeight: Float, px: Float = 6) -> Double {
        Double(max(pixelTolerance(px: px, distance: distance, viewportHeight: viewportHeight), 1e-3))
    }

    // MARK: marquee

    /// Left-to-right drags select what lies fully inside (window); right-to-
    /// left drags select anything they touch (crossing). CAD convention.
    enum MarqueeMode: Equatable {
        case window, crossing
        init(start: CGPoint, end: CGPoint) { self = end.x >= start.x ? .window : .crossing }
    }

    /// Whether an entity sampled at `points` (screen space) is caught by `rect`.
    /// Window needs every point inside; crossing needs any point inside, or a
    /// sampled segment (consecutive point pairs) passing through the rect.
    static func marqueeCatches(_ points: [CGPoint], rect: CGRect, mode: MarqueeMode,
                               segments: Bool = false) -> Bool {
        guard !points.isEmpty else { return false }
        switch mode {
        case .window: return points.allSatisfy { rect.contains($0) }
        case .crossing:
            if points.contains(where: { rect.contains($0) }) { return true }
            guard segments else { return false }
            return stride(from: 0, to: points.count - 1, by: 2).contains {
                segmentIntersectsRect(points[$0], points[$0 + 1], rect)
            }
        }
    }

    /// Liang–Barsky segment/rect test.
    static func segmentIntersectsRect(_ a: CGPoint, _ b: CGPoint, _ r: CGRect) -> Bool {
        var t0: CGFloat = 0, t1: CGFloat = 1
        let dx = b.x - a.x, dy = b.y - a.y
        for (p, q) in [(-dx, a.x - r.minX), (dx, r.maxX - a.x), (-dy, a.y - r.minY), (dy, r.maxY - a.y)] {
            if p == 0 { if q < 0 { return false }; continue }
            let t = q / p
            if p < 0 { if t > t1 { return false }; t0 = max(t0, t) }
            else { if t < t0 { return false }; t1 = min(t1, t) }
        }
        return t0 <= t1
    }

    // MARK: readout

    /// "2 faces · 1 240 mm²", "1 edge · 42.0 mm", or both joined. Nil when empty.
    static func readout(faceAreas: [Double], edgeLengths: [Double]) -> String? {
        var parts: [String] = []
        if !faceAreas.isEmpty {
            let n = faceAreas.count
            parts.append("\(n) \(n == 1 ? "face" : "faces") · \(grouped(faceAreas.reduce(0, +))) mm²")
        }
        if !edgeLengths.isEmpty {
            let n = edgeLengths.count
            parts.append("\(n) \(n == 1 ? "edge" : "edges") · \(String(format: "%.1f", edgeLengths.reduce(0, +))) mm")
        }
        return parts.isEmpty ? nil : parts.joined(separator: "   ")
    }

    /// Area with thin-space thousands grouping; one decimal below 100.
    static func grouped(_ v: Double) -> String {
        if abs(v) < 100 { return String(format: "%.1f", v) }
        let f = NumberFormatter()
        f.numberStyle = .decimal
        f.maximumFractionDigits = 0
        f.groupingSeparator = "\u{2009}"
        f.usesGroupingSeparator = true
        return f.string(from: NSNumber(value: v.rounded())) ?? String(Int(v.rounded()))
    }

    // MARK: orbit pivot

    /// What the camera should orbit around: the selected topology (face
    /// centroids / edge midpoints) first, else the selected parts' bbox
    /// centres, else nil (keep the current orbit target). Averaged.
    static func orbitPivot(topology: [SIMD3<Float>], parts: [SIMD3<Float>]) -> SIMD3<Float>? {
        let pts = topology.isEmpty ? parts : topology
        guard !pts.isEmpty else { return nil }
        return pts.reduce(.zero, +) / Float(pts.count)
    }

    /// The new orbit target after the orbit vector turns from `from` to `to`
    /// (yaw about +Y, then elevation) while rotating about `pivot` instead of
    /// the target: the camera and target swing rigidly about the pivot, so the
    /// pivot holds still on screen.
    static func panAfterOrbit(pan: SIMD3<Float>, from o0: SIMD3<Float>, to o1: SIMD3<Float>,
                              pivot: SIMD3<Float>) -> SIMD3<Float> {
        let r = rotation(from: o0, to: o1)
        let cam0 = pan + o0
        let cam1 = pivot + r.act(cam0 - pivot)
        return cam1 - o1
    }

    /// Yaw about +Y then pitch in the new vertical plane, carrying `o0` onto `o1`.
    static func rotation(from o0: SIMD3<Float>, to o1: SIMD3<Float>) -> simd_quatf {
        let a0 = atan2(o0.x, o0.z), a1 = atan2(o1.x, o1.z)
        let yaw = simd_quatf(angle: a1 - a0, axis: SIMD3(0, 1, 0))
        let mid = yaw.act(o0)
        guard simd_length(mid) > 1e-9, simd_length(o1) > 1e-9 else { return yaw }
        let pitch = simd_quatf(from: simd_normalize(mid), to: simd_normalize(o1))
        return pitch * yaw
    }
}
