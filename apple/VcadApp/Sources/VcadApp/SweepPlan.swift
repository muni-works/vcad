import Foundation
import simd

/// Placement + self-intersection check for a sketch-toolbar sweep.
///
/// The kernel lays the profile's 2D sketch coordinates out in the path's
/// moving frame: sketch (0, 0) lands on the path and the sketch plane's own
/// position is ignored. A profile drawn away from the sketch origin would
/// sweep *off* the helix by however far it was drawn from the origin, so the
/// result couldn't be predicted from the sketch. For a helix the profile is
/// therefore re-centred on its area centroid: the centroid always rides the
/// helix, which starts at (radius, 0, 0) and climbs world Z. (A straight
/// sweep is left as drawn — there the sketch coordinates already line up.)
///
/// A helix sweep self-intersects when the profile reaches the helix axis
/// (extent from the centroid ≥ radius) or when neighbouring turns touch
/// (profile size ≥ pitch, with more than one turn). The profile's rotation in
/// the path frame isn't known here, so both checks use its worst direction.
struct SweepPlan {
    let isHelix: Bool
    /// Rise per turn (length ÷ turns); 0 for a straight sweep.
    let pitch: Double
    let centroid: SIMD2<Double>
    /// Largest distance from the centroid to the profile boundary.
    let reach: Double
    /// Largest bounding-box side of the profile.
    let size: Double
    /// The profile to send: centred on its centroid for a helix, else as given.
    let profile: [String: Any]
    /// Plain-language refusal, or nil when the sweep is fine.
    let problem: String?

    init(profile input: [String: Any], turns: Double, radius: Double, length: Double) {
        isHelix = turns > 0
        pitch = isHelix ? length / turns : 0
        let segments = input["segments"] as? [[String: Any]] ?? []
        let ring = Self.sample(segments)
        let c = Self.centroid(of: ring)
        centroid = c
        reach = ring.map { simd_length($0 - c) }.max() ?? 0
        if ring.isEmpty {
            size = 0
        } else {
            let lo = ring.reduce(SIMD2(Double.infinity, .infinity)) { simd_min($0, $1) }
            let hi = ring.reduce(SIMD2(-Double.infinity, -.infinity)) { simd_max($0, $1) }
            size = max(hi.x - lo.x, hi.y - lo.y)
        }

        var p = input
        if isHelix {
            p["segments"] = segments.map { Self.shift($0, by: -c) }
            if let holes = input["holes"] as? [[[String: Any]]] {
                p["holes"] = holes.map { $0.map { Self.shift($0, by: -c) } }
            }
        }
        profile = p

        func mm(_ v: Double) -> String { String(format: "%.1f mm", v) }
        if length <= 0 {
            problem = "Sweep length must be greater than zero."
        } else if isHelix && radius <= 0 {
            problem = "Helix radius must be greater than zero."
        } else if isHelix && reach >= radius {
            problem = "The profile reaches \(mm(reach)) from its centre but the helix radius is \(mm(radius)), "
                + "so the sweep would cross the helix axis and intersect itself. Increase the radius or shrink the profile."
        } else if isHelix && turns > 1 && size >= pitch {
            problem = "The profile is \(mm(size)) across but the turns are only \(mm(pitch)) apart (length ÷ turns), "
                + "so neighbouring turns would intersect. Increase the length, use fewer turns, or shrink the profile."
        } else {
            problem = nil
        }
    }

    private static func point(_ any: Any?) -> SIMD2<Double>? {
        guard let d = any as? [String: Any], let x = (d["x"] as? NSNumber)?.doubleValue,
              let y = (d["y"] as? NSNumber)?.doubleValue else { return nil }
        return SIMD2(x, y)
    }

    private static func json(_ p: SIMD2<Double>) -> [String: Double] { ["x": p.x, "y": p.y] }

    private static func shift(_ seg: [String: Any], by d: SIMD2<Double>) -> [String: Any] {
        var s = seg
        for key in ["start", "end", "center"] { if let p = point(seg[key]) { s[key] = json(p + d) } }
        return s
    }

    /// The boundary as a polygon, arcs sampled finely.
    static func sample(_ segments: [[String: Any]]) -> [SIMD2<Double>] {
        var ring: [SIMD2<Double>] = []
        for seg in segments {
            guard let a = point(seg["start"]), let b = point(seg["end"]) else { continue }
            ring.append(a)
            guard seg["type"] as? String == "Arc", let c = point(seg["center"]) else { continue }
            let ccw = seg["ccw"] as? Bool ?? true
            let r = simd_length(a - c)
            let t0 = atan2(a.y - c.y, a.x - c.x)
            var span = atan2(b.y - c.y, b.x - c.x) - t0
            if ccw {
                while span <= 1e-12 { span += 2 * .pi }
            } else {
                while span >= -1e-12 { span -= 2 * .pi }
            }
            let n = 32
            for i in 1..<n {
                let t = t0 + span * Double(i) / Double(n)
                ring.append(c + r * SIMD2(cos(t), sin(t)))
            }
        }
        return ring
    }

    /// Area centroid of a simple polygon (vertex mean if degenerate).
    static func centroid(of ring: [SIMD2<Double>]) -> SIMD2<Double> {
        guard !ring.isEmpty else { return .zero }
        var a2 = 0.0, cx = 0.0, cy = 0.0
        for i in ring.indices {
            let p = ring[i], q = ring[(i + 1) % ring.count]
            let cr = p.x * q.y - q.x * p.y
            a2 += cr; cx += (p.x + q.x) * cr; cy += (p.y + q.y) * cr
        }
        if abs(a2) < 1e-12 { return ring.reduce(.zero, +) / Double(ring.count) }
        return SIMD2(cx, cy) / (3 * a2)
    }
}
