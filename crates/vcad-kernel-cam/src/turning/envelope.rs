//! Configuration-space envelope: the lowest nose-centre height a tool can
//! reach at each `z` without touching a set of boundary edges.
//!
//! The forbidden region for the nose centre is `edges ⊕ (−core) ⊕ disk(r)`.
//! Its upper boundary is the upper envelope of a finite set of primitives —
//! translated edges shifted by `r` along their upward normal, and radius-`r`
//! semicircles about every `q − p` vertex pair — so the envelope is computed
//! exactly (breakpoints at every primitive endpoint and pairwise
//! intersection), not sampled. Because the whole insert core takes part, a
//! wall steeper than the insert's trailing edge raises the envelope instead
//! of being gouged.
//!
//! This is the *generator's* geometry. The verifier never calls it: it
//! sweeps the tool along the posted program and measures the result
//! independently (see `verify.rs`).

use super::geom::{self, P2};

#[derive(Debug, Clone, Copy)]
enum Prim {
    /// Non-vertical segment, `z0 < z1`.
    Line { z0: f64, x0: f64, z1: f64, x1: f64 },
    /// Upper semicircle.
    Arc { cz: f64, cx: f64, r: f64 },
}

impl Prim {
    fn range(&self) -> (f64, f64) {
        match *self {
            Prim::Line { z0, z1, .. } => (z0, z1),
            Prim::Arc { cz, r, .. } => (cz - r, cz + r),
        }
    }

    fn xrange(&self) -> (f64, f64) {
        match *self {
            Prim::Line { x0, x1, .. } => (x0.min(x1), x0.max(x1)),
            Prim::Arc { cx, r, .. } => (cx, cx + r),
        }
    }

    fn eval(&self, z: f64) -> f64 {
        match *self {
            Prim::Line { z0, x0, z1, x1 } => x0 + (z - z0) / (z1 - z0) * (x1 - x0),
            Prim::Arc { cz, cx, r } => {
                let d = (z - cz).clamp(-r, r);
                cx + (r * r - d * d).max(0.0).sqrt()
            }
        }
    }

    fn contains(&self, z: f64) -> bool {
        let (a, b) = self.range();
        z >= a - 1e-12 && z <= b + 1e-12
    }
}

/// Real roots of `a t² + b t + c = 0`.
fn quad(a: f64, b: f64, c: f64) -> Vec<f64> {
    if a.abs() < 1e-14 {
        if b.abs() < 1e-14 {
            return vec![];
        }
        return vec![-c / b];
    }
    let d = b * b - 4.0 * a * c;
    if d < 0.0 {
        return vec![];
    }
    let s = d.sqrt();
    vec![(-b - s) / (2.0 * a), (-b + s) / (2.0 * a)]
}

fn intersections(p: &Prim, q: &Prim) -> Vec<f64> {
    match (*p, *q) {
        (
            Prim::Line {
                z0: a0,
                x0: b0,
                z1: a1,
                x1: b1,
            },
            Prim::Line {
                z0: c0,
                x0: d0,
                z1: c1,
                x1: d1,
            },
        ) => {
            let m1 = (b1 - b0) / (a1 - a0);
            let m2 = (d1 - d0) / (c1 - c0);
            if (m1 - m2).abs() < 1e-14 {
                return vec![];
            }
            // b0 + m1 (z − a0) = d0 + m2 (z − c0)
            vec![(d0 - b0 + m1 * a0 - m2 * c0) / (m1 - m2)]
        }
        (Prim::Line { z0, x0, z1, x1 }, Prim::Arc { cz, cx, r })
        | (Prim::Arc { cz, cx, r }, Prim::Line { z0, x0, z1, x1 }) => {
            // x = k + m z ; (z − cz)² + (x − cx)² = r²
            let m = (x1 - x0) / (z1 - z0);
            let k = x0 - m * z0 - cx;
            quad(1.0 + m * m, 2.0 * (m * k - cz), cz * cz + k * k - r * r)
        }
        (
            Prim::Arc {
                cz: a,
                cx: b,
                r: r1,
            },
            Prim::Arc {
                cz: c,
                cx: d,
                r: r2,
            },
        ) => {
            let dz = c - a;
            let dx = d - b;
            let dd = (dz * dz + dx * dx).sqrt();
            if dd < 1e-14 || dd > r1 + r2 || dd < (r1 - r2).abs() {
                return vec![];
            }
            let l = (r1 * r1 - r2 * r2 + dd * dd) / (2.0 * dd);
            let h = (r1 * r1 - l * l).max(0.0).sqrt();
            let mz = a + l * dz / dd;
            vec![mz + h * dx / dd, mz - h * dx / dd]
        }
    }
}

/// One piece of an envelope over `[z0, z1]`.
#[derive(Debug, Clone, Copy)]
struct Piece {
    z0: f64,
    z1: f64,
    prim: Option<Prim>,
}

/// Upper envelope of the forbidden region, as a function of `z`.
/// `None` pieces are free (no obstacle) and evaluate to `-∞`.
#[derive(Debug, Clone)]
pub(crate) struct Envelope {
    pieces: Vec<Piece>,
}

impl Envelope {
    /// Envelope for nose-centre `C` of a tool with `core` (relative to `C`)
    /// and radius `r`, against boundary `edges`, over `[z_lo, z_hi]`.
    pub(crate) fn new(edges: &[(P2, P2)], core: &[P2], r: f64, z_lo: f64, z_hi: f64) -> Self {
        let mut prims: Vec<Prim> = Vec::new();
        let core_edges = geom::poly_edges(core);
        let push_seg = |u: P2, w: P2, prims: &mut Vec<Prim>| {
            let (u, w) = if u[0] <= w[0] { (u, w) } else { (w, u) };
            let dz = w[0] - u[0];
            if dz > 1e-12 {
                let len = geom::dist(u, w);
                // Upward unit normal.
                let n = [-(w[1] - u[1]) / len, dz / len];
                prims.push(Prim::Line {
                    z0: u[0] + r * n[0],
                    x0: u[1] + r * n[1],
                    z1: w[0] + r * n[0],
                    x1: w[1] + r * n[1],
                });
            }
            if r > 0.0 {
                for c in [u, w] {
                    prims.push(Prim::Arc {
                        cz: c[0],
                        cx: c[1],
                        r,
                    });
                }
            } else {
                // Degenerate radius: a point obstacle is a zero-width line.
                for c in [u, w] {
                    prims.push(Prim::Line {
                        z0: c[0] - 1e-9,
                        x0: c[1],
                        z1: c[0] + 1e-9,
                        x1: c[1],
                    });
                }
            }
        };
        // Reach of the tool beyond its nose centre, to cull far edges.
        let cb = geom::bbox(core);
        let (lo, hi) = (z_lo - cb[2] - r - 1.0, z_hi - cb[0] + r + 1.0);
        for &(a, b) in edges {
            if a[0].max(b[0]) < lo || a[0].min(b[0]) > hi {
                continue;
            }
            for p in core {
                push_seg(geom::sub(a, *p), geom::sub(b, *p), &mut prims);
            }
            for q in [a, b] {
                for &(p0, p1) in &core_edges {
                    push_seg(geom::sub(q, p0), geom::sub(q, p1), &mut prims);
                }
            }
        }
        // Breakpoints.
        let mut zs: Vec<f64> = vec![z_lo, z_hi];
        for p in &prims {
            let (a, b) = p.range();
            zs.push(a);
            zs.push(b);
        }
        let mut order: Vec<usize> = (0..prims.len()).collect();
        order.sort_by(|&i, &j| prims[i].range().0.total_cmp(&prims[j].range().0));
        for (k, &i) in order.iter().enumerate() {
            let (a0, a1) = prims[i].range();
            let (ax0, ax1) = prims[i].xrange();
            for &j in &order[k + 1..] {
                let (b0, b1) = prims[j].range();
                if b0 > a1 {
                    break;
                }
                let (bx0, bx1) = prims[j].xrange();
                if bx0 > ax1 || ax0 > bx1 {
                    continue;
                }
                for z in intersections(&prims[i], &prims[j]) {
                    if z >= a0.max(b0) && z <= a1.min(b1) {
                        zs.push(z);
                    }
                }
            }
        }
        zs.retain(|z| *z >= z_lo && *z <= z_hi && z.is_finite());
        zs.sort_by(f64::total_cmp);
        zs.dedup_by(|a, b| (*a - *b).abs() < 1e-10);

        let mut pieces: Vec<Piece> = Vec::new();
        for w in zs.windows(2) {
            let m = 0.5 * (w[0] + w[1]);
            let mut best: Option<(f64, Prim)> = None;
            for p in &prims {
                if p.contains(m) {
                    let v = p.eval(m);
                    if best.is_none_or(|(bv, _)| v > bv) {
                        best = Some((v, *p));
                    }
                }
            }
            let prim = best.map(|b| b.1);
            if let Some(last) = pieces.last_mut() {
                let same = match (last.prim, prim) {
                    (None, None) => true,
                    (Some(a), Some(b)) => prim_eq(&a, &b),
                    _ => false,
                };
                if same {
                    last.z1 = w[1];
                    continue;
                }
            }
            pieces.push(Piece {
                z0: w[0],
                z1: w[1],
                prim,
            });
        }
        Envelope { pieces }
    }

    /// Envelope value at `z` (`-∞` where free). At a jump, the larger side.
    pub(crate) fn value(&self, z: f64) -> f64 {
        let mut v = f64::NEG_INFINITY;
        for p in &self.pieces {
            if z >= p.z0 - 1e-12 && z <= p.z1 + 1e-12 {
                if let Some(pr) = p.prim {
                    v = v.max(pr.eval(z));
                }
            }
        }
        v
    }

    /// Minimum of `max(value, floor)` over `[a, b]`.
    pub(crate) fn min_over(&self, a: f64, b: f64, floor: f64) -> f64 {
        let mut m = f64::INFINITY;
        for p in &self.pieces {
            let (z0, z1) = (p.z0.max(a), p.z1.min(b));
            if z0 > z1 {
                continue;
            }
            let v = match p.prim {
                None => floor,
                Some(pr @ Prim::Line { .. }) => pr.eval(z0).min(pr.eval(z1)).max(floor),
                Some(pr @ Prim::Arc { .. }) => pr.eval(z0).min(pr.eval(z1)).max(floor),
            };
            m = m.min(v);
        }
        m
    }

    /// Sub-intervals of `[a, b]` where `value ≤ x`, ascending, merged.
    pub(crate) fn below(&self, x: f64, a: f64, b: f64) -> Vec<(f64, f64)> {
        let mut out: Vec<(f64, f64)> = Vec::new();
        let add = |lo: f64, hi: f64, out: &mut Vec<(f64, f64)>| {
            if hi < lo {
                return;
            }
            if let Some(last) = out.last_mut() {
                if lo <= last.1 + 1e-9 {
                    last.1 = last.1.max(hi);
                    return;
                }
            }
            out.push((lo, hi));
        };
        for p in &self.pieces {
            let (z0, z1) = (p.z0.max(a), p.z1.min(b));
            if z0 > z1 {
                continue;
            }
            match p.prim {
                None => add(z0, z1, &mut out),
                Some(pr) => {
                    // Split at the roots of pr(z) = x, then test midpoints.
                    let mut cuts = vec![z0, z1];
                    let probe = Prim::Line {
                        z0: z0 - 1.0,
                        x0: x,
                        z1: z1 + 1.0,
                        x1: x,
                    };
                    for z in intersections(&pr, &probe) {
                        if z > z0 && z < z1 {
                            cuts.push(z);
                        }
                    }
                    cuts.sort_by(f64::total_cmp);
                    for w in cuts.windows(2) {
                        let m = 0.5 * (w[0] + w[1]);
                        if pr.eval(m) <= x + 1e-9 {
                            add(w[0], w[1], &mut out);
                        }
                    }
                }
            }
        }
        out
    }

    /// Nose-centre path following `max(value, floor)` from `z_hi` down to
    /// `z_lo` (descending `z`). Arcs are chorded on the *outside* so the path
    /// never dips under the envelope; sagitta ≤ `tol`.
    pub(crate) fn path(&self, z_hi: f64, z_lo: f64, floor: f64, tol: f64) -> Vec<P2> {
        let mut pts: Vec<P2> = Vec::new();
        let push = |p: P2, pts: &mut Vec<P2>| {
            if let Some(l) = pts.last() {
                if geom::dist(*l, p) < 1e-9 {
                    return;
                }
            }
            pts.push(p);
        };
        for p in self.pieces.iter().rev() {
            let (z0, z1) = (p.z0.max(z_lo), p.z1.min(z_hi));
            if z0 >= z1 {
                continue;
            }
            match p.prim {
                None => {
                    push([z1, floor], &mut pts);
                    push([z0, floor], &mut pts);
                }
                Some(Prim::Line { .. }) => {
                    let pr = p.prim.unwrap();
                    let (v1, v0) = (pr.eval(z1), pr.eval(z0));
                    if v1 >= floor && v0 >= floor {
                        push([z1, v1], &mut pts);
                        push([z0, v0], &mut pts);
                    } else {
                        // Clip against the floor.
                        let zc = if (v1 - v0).abs() > 1e-15 {
                            z0 + (floor - v0) / (v1 - v0) * (z1 - z0)
                        } else {
                            z0
                        };
                        push([z1, v1.max(floor)], &mut pts);
                        if zc > z0 && zc < z1 {
                            push([zc, floor], &mut pts);
                        }
                        push([z0, v0.max(floor)], &mut pts);
                    }
                }
                Some(Prim::Arc { cz, cx, r }) => {
                    let a1 = ((z1 - cz) / r).clamp(-1.0, 1.0).acos();
                    let a0 = ((z0 - cz) / r).clamp(-1.0, 1.0).acos();
                    let sweep = a0 - a1;
                    let step_max = 2.0 * (1.0 - tol.min(r) / r).clamp(-1.0, 1.0).acos();
                    let n = ((sweep / step_max.max(1e-6)).ceil() as usize).max(1);
                    let step = sweep / n as f64;
                    let ro = r / (step / 2.0).cos();
                    push([z1, (cx + r * a1.sin()).max(floor)], &mut pts);
                    for i in 0..n {
                        let am = a1 + step * (i as f64 + 0.5);
                        // Vertices of the circumscribed polygon.
                        let p = [cz + ro * am.cos(), (cx + ro * am.sin()).max(floor)];
                        if p[0] < z1 && p[0] > z0 {
                            push(p, &mut pts);
                        }
                    }
                    push([z0, (cx + r * a0.sin()).max(floor)], &mut pts);
                }
            }
        }
        // Make every jump vertical at the higher side so the descending path
        // never cuts a corner under a wall.
        let mut out: Vec<P2> = Vec::new();
        for p in pts {
            if let Some(&l) = out.last() {
                let dz = l[0] - p[0];
                let dx = p[1] - l[1];
                if dz.abs() < 1e-9 && dx.abs() > 1e-9 {
                    out.push(p);
                    continue;
                }
                // A rise steeper than the envelope allows: step up first.
                let mid_ok = {
                    let m = [0.5 * (l[0] + p[0]), 0.5 * (l[1] + p[1])];
                    m[1] + 1e-7 >= self.value(m[0]).max(floor)
                };
                if !mid_ok {
                    let hi = l[1].max(p[1]);
                    if l[1] < hi {
                        out.push([l[0], hi]);
                    } else {
                        out.push([p[0], hi]);
                    }
                }
            }
            out.push(p);
        }
        out
    }
}

fn prim_eq(a: &Prim, b: &Prim) -> bool {
    match (a, b) {
        (
            Prim::Line { z0, x0, z1, x1 },
            Prim::Line {
                z0: c0,
                x0: d0,
                z1: c1,
                x1: d1,
            },
        ) => z0 == c0 && x0 == d0 && z1 == c1 && x1 == d1,
        (Prim::Arc { cz, cx, r }, Prim::Arc { cz: a, cx: b, r: c }) => cz == a && cx == b && r == c,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_line_with_disk_tool() {
        // Horizontal edge at x = 5 from z = −10..0; disk tool r = 0.5.
        let e = Envelope::new(
            &[([-10.0, 5.0], [0.0, 5.0])],
            &[[0.0, 0.0]],
            0.5,
            -12.0,
            2.0,
        );
        assert!((e.value(-5.0) - 5.5).abs() < 1e-12);
        // Rounds the corner at z = 0.
        assert!((e.value(0.3) - (5.0 + (0.25f64 - 0.09).sqrt())).abs() < 1e-12);
        assert_eq!(e.value(1.0), f64::NEG_INFINITY);
        let iv = e.below(5.2, -12.0, 2.0);
        assert_eq!(iv.len(), 2);
    }

    #[test]
    fn wedge_lifts_over_steep_descending_wall() {
        // OD rising toward the chuck: step down from r=8 (z<−5) to r=5.
        let edges = [
            ([-10.0, 8.0], [-5.0, 8.0]),
            ([-5.0, 8.0], [-5.0, 5.0]),
            ([-5.0, 5.0], [0.0, 5.0]),
        ];
        // A wedge whose trailing edge points up-left (a left-hand style
        // body) cannot sit right against the wall's foot.
        let core = [[0.0, 0.0], [-3.0, 3.0], [0.0, 3.0]];
        let e = Envelope::new(&edges, &core, 0.2, -12.0, 2.0);
        // Far from the wall: nose rides at 5.2.
        assert!((e.value(-1.0) - 5.2).abs() < 1e-9);
        // Within the body's reach of the wall top the envelope rises.
        assert!(e.value(-4.0) > 5.2 + 1e-6);
    }
}
