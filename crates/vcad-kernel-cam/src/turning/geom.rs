//! Small 2D geometry kit for the turning half-plane.
//!
//! Every turning computation lives in the (z, x) plane through the spindle
//! axis: `z` along the axis (positive toward the tailstock), `x` the radius.
//! Points are `[z, x]`.

/// A point or vector in the turning plane, `[z, x]`.
pub type P2 = [f64; 2];

#[inline]
pub(crate) fn add(a: P2, b: P2) -> P2 {
    [a[0] + b[0], a[1] + b[1]]
}

#[inline]
pub(crate) fn sub(a: P2, b: P2) -> P2 {
    [a[0] - b[0], a[1] - b[1]]
}

#[inline]
pub(crate) fn scale(a: P2, s: f64) -> P2 {
    [a[0] * s, a[1] * s]
}

#[inline]
pub(crate) fn dot(a: P2, b: P2) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

#[inline]
pub(crate) fn cross(a: P2, b: P2) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}

#[inline]
pub(crate) fn norm(a: P2) -> f64 {
    dot(a, a).sqrt()
}

#[inline]
pub(crate) fn dist(a: P2, b: P2) -> f64 {
    norm(sub(a, b))
}

/// Distance from `p` to the segment `a`–`b`.
pub(crate) fn point_seg_dist(p: P2, a: P2, b: P2) -> f64 {
    let ab = sub(b, a);
    let l2 = dot(ab, ab);
    if l2 <= 0.0 {
        return dist(p, a);
    }
    let t = (dot(sub(p, a), ab) / l2).clamp(0.0, 1.0);
    dist(p, add(a, scale(ab, t)))
}

/// Do the closed segments `a`–`b` and `c`–`d` intersect (touching counts)?
pub(crate) fn segs_intersect(a: P2, b: P2, c: P2, d: P2) -> bool {
    let d1 = cross(sub(b, a), sub(c, a));
    let d2 = cross(sub(b, a), sub(d, a));
    let d3 = cross(sub(d, c), sub(a, c));
    let d4 = cross(sub(d, c), sub(b, c));
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return true;
    }
    let on = |p: P2, q: P2, r: P2, d: f64| {
        d == 0.0
            && r[0] >= p[0].min(q[0])
            && r[0] <= p[0].max(q[0])
            && r[1] >= p[1].min(q[1])
            && r[1] <= p[1].max(q[1])
    };
    on(a, b, c, d1) || on(a, b, d, d2) || on(c, d, a, d3) || on(c, d, b, d4)
}

/// Distance between segments (0 when they intersect).
pub(crate) fn seg_seg_dist(a: P2, b: P2, c: P2, d: P2) -> f64 {
    if segs_intersect(a, b, c, d) {
        return 0.0;
    }
    point_seg_dist(a, c, d)
        .min(point_seg_dist(b, c, d))
        .min(point_seg_dist(c, a, b))
        .min(point_seg_dist(d, a, b))
}

/// Convex hull (Andrew's monotone chain), counter-clockwise, no collinear
/// points. Returns the input for fewer than three distinct points.
pub(crate) fn convex_hull(points: &[P2]) -> Vec<P2> {
    let mut pts: Vec<P2> = points.to_vec();
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-12 && (a[1] - b[1]).abs() < 1e-12);
    if pts.len() < 3 {
        return pts;
    }
    let mut lower: Vec<P2> = Vec::new();
    for &p in &pts {
        while lower.len() >= 2
            && cross(
                sub(lower[lower.len() - 1], lower[lower.len() - 2]),
                sub(p, lower[lower.len() - 2]),
            ) <= 0.0
        {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<P2> = Vec::new();
    for &p in pts.iter().rev() {
        while upper.len() >= 2
            && cross(
                sub(upper[upper.len() - 1], upper[upper.len() - 2]),
                sub(p, upper[upper.len() - 2]),
            ) <= 0.0
        {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Is `p` inside (or on) the convex CCW polygon `poly`?
pub(crate) fn in_convex(poly: &[P2], p: P2) -> bool {
    match poly.len() {
        0 => false,
        1 => dist(poly[0], p) < 1e-12,
        2 => point_seg_dist(p, poly[0], poly[1]) < 1e-12,
        n => (0..n).all(|i| cross(sub(poly[(i + 1) % n], poly[i]), sub(p, poly[i])) >= -1e-12),
    }
}

/// Distance from `p` to the convex CCW polygon `poly` (0 inside).
pub(crate) fn point_convex_dist(poly: &[P2], p: P2) -> f64 {
    if in_convex(poly, p) {
        return 0.0;
    }
    let n = poly.len();
    if n == 1 {
        return dist(poly[0], p);
    }
    (0..n)
        .map(|i| point_seg_dist(p, poly[i], poly[(i + 1) % n]))
        .fold(f64::INFINITY, f64::min)
}

/// Edges of a polygon as segment pairs (a 2-point polygon is one segment).
pub(crate) fn poly_edges(poly: &[P2]) -> Vec<(P2, P2)> {
    match poly.len() {
        0 => vec![],
        1 => vec![(poly[0], poly[0])],
        2 => vec![(poly[0], poly[1])],
        n => (0..n).map(|i| (poly[i], poly[(i + 1) % n])).collect(),
    }
}

/// Axis-aligned bounding box `[zmin, xmin, zmax, xmax]`.
pub(crate) fn bbox(points: &[P2]) -> [f64; 4] {
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for p in points {
        b[0] = b[0].min(p[0]);
        b[1] = b[1].min(p[1]);
        b[2] = b[2].max(p[0]);
        b[3] = b[3].max(p[1]);
    }
    b
}

/// Even-odd point-in-region test over a closed edge soup.
pub(crate) fn in_region(edges: &[(P2, P2)], p: P2) -> bool {
    let mut inside = false;
    for &(a, b) in edges {
        if (a[1] > p[1]) != (b[1] > p[1]) {
            let z = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if z > p[0] {
                inside = !inside;
            }
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hull_and_containment() {
        let h = convex_hull(&[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.5, 0.5]]);
        assert_eq!(h.len(), 4);
        assert!(in_convex(&h, [0.5, 0.5]));
        assert!(!in_convex(&h, [1.5, 0.5]));
        assert!((point_convex_dist(&h, [2.0, 0.5]) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn segment_distance() {
        assert_eq!(
            seg_seg_dist([0.0, 0.0], [1.0, 1.0], [0.0, 1.0], [1.0, 0.0]),
            0.0
        );
        let d = seg_seg_dist([0.0, 0.0], [1.0, 0.0], [0.0, 2.0], [1.0, 2.0]);
        assert!((d - 2.0).abs() < 1e-12);
    }

    #[test]
    fn region_parity() {
        let sq = [[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]];
        let e = poly_edges(&sq);
        assert!(in_region(&e, [1.0, 1.0]));
        assert!(!in_region(&e, [3.0, 1.0]));
    }
}
