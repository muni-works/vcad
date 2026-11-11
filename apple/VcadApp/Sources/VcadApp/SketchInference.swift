import Foundation
import simd

// Pure sketch logic, solver-free so it can be tested directly:
//  - SketchInference: what a drawing click snaps to and which constraints it implies
//  - SmartDimension: which dimension a pick (or pair of picks) means
//  - SketchFill: triangulating closed profiles for the live shading
//  - SketchEdit: trim / extend / fillet / mirror / pattern geometry

typealias V2 = SIMD2<Double>

// MARK: - inference

/// A constraint implied by where the cursor landed.
enum SketchInferred: Hashable {
    case coincident(UInt32)      // point handle
    case midpoint(UInt32)        // line handle
    case onLine(UInt32)          // line handle
    case onCircle(UInt32)        // circle/arc handle
    case horizontal, vertical
    case perpendicular(UInt32)   // line handle
    case parallel(UInt32)        // line handle
    case equal(UInt32)           // line handle
    case tangent(UInt32)         // arc/circle handle, at the anchor point

    var glyph: String {
        switch self {
        case .coincident: "●"
        case .midpoint: "M"
        case .onLine, .onCircle: "∈"
        case .horizontal: "H"
        case .vertical: "V"
        case .perpendicular: "⊥"
        case .parallel: "∥"
        case .equal: "="
        case .tangent: "T"
        }
    }
}

struct SketchInference {
    struct Line { var handle: UInt32; var a: V2; var b: V2; var aH: UInt32; var bH: UInt32 }
    struct Curve { var handle: UInt32; var center: V2; var radius: Double; var ends: [UInt32] }

    var points: [(UInt32, V2)] = []
    var lines: [Line] = []
    var curves: [Curve] = []
    /// Snap reach in plane units.
    var reach: Double = 1
    /// Angle tolerance (radians) for H/V/⊥/∥/tangent.
    var angleTol: Double = 0.05

    struct Result: Equatable {
        var point: V2
        var inferred: [SketchInferred]
        var snapped: UInt32? {
            for i in inferred { if case .coincident(let h) = i { return h } }
            return nil
        }
    }

    /// Where a click at `p` lands for a segment starting at `anchor` (handle
    /// `anchorH`), with `previous` the segment drawn just before (for ⊥/∥/=).
    func infer(_ p: V2, anchor: V2? = nil, anchorH: UInt32? = nil, previous: UInt32? = nil) -> Result {
        var out: [SketchInferred] = []
        var q = p
        var placed = false

        // 1. An existing point.
        if let hit = points.filter({ $0.0 != anchorH })
            .map({ ($0.0, $0.1, simd_distance($0.1, p)) }).filter({ $0.2 <= reach }).min(by: { $0.2 < $1.2 }) {
            q = hit.1; out.append(.coincident(hit.0)); placed = true
        }
        // 2. A line's midpoint.
        if !placed, let m = lines.filter({ $0.aH != anchorH && $0.bH != anchorH })
            .map({ ($0.handle, ($0.a + $0.b) / 2) }).first(where: { simd_distance($0.1, p) <= reach }) {
            q = m.1; out.append(.midpoint(m.0)); placed = true
        }

        // 3. Direction from the anchor.
        var directional = false
        if let a = anchor, simd_distance(a, q) > 1e-9 {
            let d = q - a, len = simd_length(d)
            let ang = atan2(d.y, d.x)
            func near(_ x: Double, _ y: Double) -> Bool { abs(remainder(x - y, .pi)) < angleTol }
            var dir: V2?
            if near(ang, 0) { out.append(.horizontal); dir = V2(d.x < 0 ? -1 : 1, 0) }
            else if near(ang, .pi / 2) { out.append(.vertical); dir = V2(0, d.y < 0 ? -1 : 1) }
            else if let ph = previous, let pl = lines.first(where: { $0.handle == ph }), simd_length(pl.b - pl.a) > 1e-9 {
                let t = simd_normalize(pl.b - pl.a), pa = atan2(t.y, t.x)
                if near(ang, pa + .pi / 2) {
                    out.append(.perpendicular(ph)); let n = V2(-t.y, t.x); dir = simd_dot(n, d) < 0 ? -n : n
                } else if near(ang, pa) {
                    out.append(.parallel(ph)); dir = simd_dot(t, d) < 0 ? -t : t
                }
            }
            if dir == nil, let ah = anchorH, let c = curves.first(where: { $0.ends.contains(ah) }) {
                let r = a - c.center
                if simd_length(r) > 1e-9, abs(simd_dot(simd_normalize(r), d / len)) < sin(angleTol) {
                    out.append(.tangent(c.handle)); let t = V2(-r.y, r.x) / simd_length(r)
                    dir = simd_dot(t, d) < 0 ? -t : t
                }
            }
            if let dir {
                directional = true
                if !placed {
                    var l = simd_dot(q - a, dir)
                    // Equal length to the previous segment.
                    if let ph = previous, let pl = lines.first(where: { $0.handle == ph }) {
                        let pl0 = simd_distance(pl.a, pl.b)
                        if abs(l - pl0) <= reach { l = pl0; out.append(.equal(ph)) }
                    }
                    q = a + dir * l
                }
            }
        }

        // 4. On a line or circle (only when nothing stronger fixed the point).
        if !placed && !directional {
            if let l = lines.filter({ $0.aH != anchorH && $0.bH != anchorH })
                .first(where: { Self.segDist(p, $0.a, $0.b) <= reach * 0.6 }) {
                let ab = l.b - l.a, t = simd_dot(p - l.a, ab) / max(simd_length_squared(ab), 1e-12)
                q = l.a + t * ab; out.append(.onLine(l.handle))
            } else if let c = curves.first(where: { abs(simd_distance($0.center, p) - $0.radius) <= reach * 0.6 }),
                      simd_distance(c.center, p) > 1e-9 {
                q = c.center + simd_normalize(p - c.center) * c.radius; out.append(.onCircle(c.handle))
            }
        }
        return Result(point: q, inferred: out)
    }

    static func segDist(_ p: V2, _ a: V2, _ b: V2) -> Double {
        let ab = b - a, l2 = simd_length_squared(ab)
        guard l2 > 1e-12 else { return simd_distance(p, a) }
        return simd_distance(p, a + max(0, min(1, simd_dot(p - a, ab) / l2)) * ab)
    }

}

// MARK: - smart dimension

/// What one click of the Smart Dimension tool picked.
enum SketchPick: Equatable {
    case point(UInt32, V2)
    case line(UInt32, V2, V2)
    /// A circle (`full`) or arc, with its center point handle.
    case curve(UInt32, center: UInt32, V2, radius: Double, full: Bool)
}

struct DimensionPlan: Equatable {
    var kind: SketchConstraintKind
    var e0: UInt32
    var e1: UInt32 = 0
    var value: Double
}

enum SmartDimension {
    /// The dimension a pick means alone (`second == nil`) or with a second pick.
    static func plan(_ a: SketchPick, _ b: SketchPick? = nil) -> DimensionPlan? {
        switch (a, b) {
        case (.line(let h, let p, let q), nil):
            return .init(kind: .length, e0: h, value: simd_distance(p, q))
        case (.curve(let h, _, _, let r, let full), nil):
            return full ? .init(kind: .diameter, e0: h, value: 2 * r) : .init(kind: .radius, e0: h, value: r)
        case (.point, nil):
            return nil
        case (.point(let h, let p), .point(let k, let q)):
            return h == k ? nil : .init(kind: .distance, e0: h, e1: k, value: simd_distance(p, q))
        case (.point(let h, let p), .line(let l, let a, let b)), (.line(let l, let a, let b), .point(let h, let p)):
            return .init(kind: .pointLineDistance, e0: h, e1: l, value: lineDistance(p, a, b))
        case (.line(let h, let a0, let a1), .line(let k, let b0, let b1)):
            if h == k { return plan(a) }
            let da = a1 - a0, db = b1 - b0
            guard simd_length(da) > 1e-9, simd_length(db) > 1e-9 else { return nil }
            let cr = (da.x * db.y - da.y * db.x) / (simd_length(da) * simd_length(db))
            if abs(cr) < 1e-6 { return nil }   // parallel: no line-line distance in the solver
            // The solver's angle is signed (a → b, counter-clockwise): order
            // the pair so the value reads positive and nothing flips.
            let deg = signedAngle(da, db)
            return deg >= 0 ? .init(kind: .angle, e0: h, e1: k, value: deg) : .init(kind: .angle, e0: k, e1: h, value: -deg)
        case (.curve(let h, let c, let p, _, _), .curve(let k, let d, let q, _, _)):
            if h == k { return plan(a) }
            return c == d ? nil : .init(kind: .distance, e0: c, e1: d, value: simd_distance(p, q))
        case (.point(let h, let p), .curve(_, let c, let q, _, _)), (.curve(_, let c, let q, _, _), .point(let h, let p)):
            return h == c ? nil : .init(kind: .distance, e0: h, e1: c, value: simd_distance(p, q))
        case (.line(let l, let a0, let a1), .curve(_, let c, let q, _, _)), (.curve(_, let c, let q, _, _), .line(let l, let a0, let a1)):
            return .init(kind: .pointLineDistance, e0: c, e1: l, value: lineDistance(q, a0, a1))
        default:
            return nil
        }
    }

    /// Counter-clockwise angle from a to b in degrees, (-180, 180].
    static func signedAngle(_ a: V2, _ b: V2) -> Double {
        atan2(a.x * b.y - a.y * b.x, simd_dot(a, b)) * 180 / .pi
    }

    static func lineDistance(_ p: V2, _ a: V2, _ b: V2) -> Double {
        let d = b - a, l = simd_length(d)
        guard l > 1e-12 else { return simd_distance(p, a) }
        return abs(d.x * (p.y - a.y) - d.y * (p.x - a.x)) / l
    }
}

// MARK: - profile fill

enum SketchFill {
    /// Signed area (CCW positive).
    static func area(_ poly: [V2]) -> Double {
        guard poly.count >= 3 else { return 0 }
        return zip(poly, poly.dropFirst() + [poly[0]]).reduce(0) { $0 + ($1.0.x * $1.1.y - $1.1.x * $1.0.y) } / 2
    }

    /// Bridge holes into the outer loop so one ear-clip covers the region.
    static func bridge(outer: [V2], holes: [[V2]]) -> [V2] {
        var poly = area(outer) < 0 ? Array(outer.reversed()) : outer
        // Rightmost hole first, each joined to the nearest visible outer vertex.
        let sorted = holes.filter { $0.count >= 3 }
            .map { area($0) > 0 ? Array($0.reversed()) : $0 }
            .sorted { ($0.map(\.x).max() ?? 0) > ($1.map(\.x).max() ?? 0) }
        for h in sorted {
            guard let hi = h.indices.max(by: { h[$0].x < h[$1].x }) else { continue }
            let m = h[hi]
            let candidates = poly.indices.sorted { simd_distance(poly[$0], m) < simd_distance(poly[$1], m) }
            let pi = candidates.first { i in !crossesAny(m, poly[i], poly) && !crossesAny(m, poly[i], h) } ?? candidates[0]
            let rotated = Array(h[hi...] + h[..<hi])
            poly = Array(poly[...pi]) + rotated + [m, poly[pi]] + Array(poly[(pi + 1)...])
        }
        return poly
    }

    private static func crossesAny(_ p: V2, _ q: V2, _ loop: [V2]) -> Bool {
        for i in loop.indices {
            let a = loop[i], b = loop[(i + 1) % loop.count]
            if a == p || a == q || b == p || b == q { continue }
            if properCross(p, q, a, b) { return true }
        }
        return false
    }

    static func properCross(_ p: V2, _ q: V2, _ a: V2, _ b: V2) -> Bool {
        func o(_ x: V2, _ y: V2, _ z: V2) -> Double { (y.x - x.x) * (z.y - x.y) - (y.y - x.y) * (z.x - x.x) }
        let d1 = o(p, q, a), d2 = o(p, q, b), d3 = o(a, b, p), d4 = o(a, b, q)
        return d1 * d2 < 0 && d3 * d4 < 0
    }

    /// Ear-clipping triangulation of a simple polygon (either winding).
    /// Returns triangles as index triples into `poly`.
    static func triangulate(_ poly: [V2]) -> [(Int, Int, Int)] {
        guard poly.count >= 3 else { return [] }
        var idx = Array(poly.indices)
        if area(poly) < 0 { idx.reverse() }
        var tris: [(Int, Int, Int)] = []
        func cross(_ a: V2, _ b: V2, _ c: V2) -> Double { (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x) }
        func inside(_ p: V2, _ a: V2, _ b: V2, _ c: V2) -> Bool {
            cross(a, b, p) >= -1e-12 && cross(b, c, p) >= -1e-12 && cross(c, a, p) >= -1e-12
        }
        var guardN = idx.count * idx.count + 10
        while idx.count > 3 && guardN > 0 {
            guardN -= 1
            var clipped = false
            for i in idx.indices {
                let ia = idx[(i + idx.count - 1) % idx.count], ib = idx[i], ic = idx[(i + 1) % idx.count]
                let a = poly[ia], b = poly[ib], c = poly[ic]
                guard cross(a, b, c) > 1e-12 else { continue }
                let blocked = idx.contains { j in
                    j != ia && j != ib && j != ic && poly[j] != a && poly[j] != b && poly[j] != c && inside(poly[j], a, b, c)
                }
                if blocked { continue }
                tris.append((ia, ib, ic)); idx.remove(at: i); clipped = true; break
            }
            if !clipped {   // degenerate remainder: drop a collinear vertex
                if let i = idx.indices.first(where: { i in
                    let a = poly[idx[(i + idx.count - 1) % idx.count]], b = poly[idx[i]], c = poly[idx[(i + 1) % idx.count]]
                    return abs(cross(a, b, c)) <= 1e-12
                }) { idx.remove(at: i) } else { break }
            }
        }
        if idx.count == 3, cross(poly[idx[0]], poly[idx[1]], poly[idx[2]]) > 1e-12 { tris.append((idx[0], idx[1], idx[2])) }
        return tris
    }
}

// MARK: - editing geometry

enum SketchEdit {
    /// Parameters t ∈ (0,1) along a→b where it crosses any of `cutters`.
    static func crossings(_ a: V2, _ b: V2, cutters: [(V2, V2)]) -> [Double] {
        let r = b - a
        var ts: [Double] = []
        for (c, d) in cutters {
            let s = d - c, den = r.x * s.y - r.y * s.x
            guard abs(den) > 1e-12 else { continue }
            let t = ((c.x - a.x) * s.y - (c.y - a.y) * s.x) / den
            let u = ((c.x - a.x) * r.y - (c.y - a.y) * r.x) / den
            if t > 1e-9, t < 1 - 1e-9, u >= -1e-9, u <= 1 + 1e-9 { ts.append(t) }
        }
        return ts.sorted()
    }

    /// Trim the piece of segment a→b around parameter `at`: the kept pieces
    /// (zero, one or two) as parameter ranges.
    static func trim(_ a: V2, _ b: V2, at: Double, cutters: [(V2, V2)]) -> [ClosedRange<Double>] {
        let ts = crossings(a, b, cutters: cutters)
        let lo = ts.last { $0 < at } ?? 0, hi = ts.first { $0 > at } ?? 1
        var keep: [ClosedRange<Double>] = []
        if lo > 0 { keep.append(0...lo) }
        if hi < 1 { keep.append(hi...1) }
        return keep
    }

    /// Extend the end of a→b nearest `near` to the closest cutter it would
    /// hit; nil when nothing lies ahead.
    static func extend(_ a: V2, _ b: V2, near: V2, cutters: [(V2, V2)]) -> V2? {
        let fromEnd = simd_distance(near, b) <= simd_distance(near, a)
        let (p, q) = fromEnd ? (a, b) : (b, a)
        let r = q - p
        var best: Double?
        for (c, d) in cutters {
            let s = d - c, den = r.x * s.y - r.y * s.x
            guard abs(den) > 1e-12 else { continue }
            let t = ((c.x - p.x) * s.y - (c.y - p.y) * s.x) / den
            let u = ((c.x - p.x) * r.y - (c.y - p.y) * r.x) / den
            if t > 1 + 1e-9, u >= -1e-9, u <= 1 + 1e-9, t < (best ?? .infinity) { best = t }
        }
        return best.map { p + $0 * r }
    }

    struct Fillet: Equatable { var onA: V2; var onB: V2; var center: V2; var ccw: Bool }

    /// Round the corner `c` between rays c→a and c→b with radius r.
    static func fillet(corner c: V2, a: V2, b: V2, radius r: Double) -> Fillet? {
        let ua = a - c, ub = b - c
        guard simd_length(ua) > 1e-9, simd_length(ub) > 1e-9, r > 0 else { return nil }
        let da = simd_normalize(ua), db = simd_normalize(ub)
        let half = acos(max(-1, min(1, simd_dot(da, db)))) / 2
        guard half > 1e-6, half < .pi / 2 - 1e-6 else { return nil }
        let t = r / tan(half)
        guard t < simd_length(ua), t < simd_length(ub) else { return nil }
        let bis = simd_normalize(da + db)
        let center = c + bis * (r / sin(half))
        let onA = c + da * t, onB = c + db * t
        let s = (onA - center), e = (onB - center)
        return Fillet(onA: onA, onB: onB, center: center, ccw: s.x * e.y - s.y * e.x > 0)
    }

    /// Mirror p across the line through a, b.
    static func mirror(_ p: V2, _ a: V2, _ b: V2) -> V2 {
        let d = b - a, l2 = simd_length_squared(d)
        guard l2 > 1e-12 else { return p }
        let foot = a + d * (simd_dot(p - a, d) / l2)
        return 2 * foot - p
    }

    /// Rotate p about c by `angle` radians.
    static func rotate(_ p: V2, about c: V2, by angle: Double) -> V2 {
        let d = p - c
        return c + V2(d.x * cos(angle) - d.y * sin(angle), d.x * sin(angle) + d.y * cos(angle))
    }
}
