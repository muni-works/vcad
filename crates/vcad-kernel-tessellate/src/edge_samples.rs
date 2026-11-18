//! Shared edge sampling: every B-rep edge is discretized ONCE, and every
//! face bounded by it reads the same points.
//!
//! The topology stores no curve on an edge — a half-edge is just two
//! vertices — so a curved rim reaches the tessellator either as a dense
//! vertex chain (boolean results, frozen onto the canonical arc grid) or as
//! a coarse one (a sketch extrude stores each arc as an 8-point chain).
//! Faces then disagreed about what lies between two chain vertices: a
//! planar cap rendered the chord, while the curved wall beside it resampled
//! the true arc at the tessellation density. Every such edge opened a
//! sag-wide crack — 144 open edges on an arc-extruded D at 128 segments.
//!
//! This module recovers the edge's curve from the two surfaces that meet
//! along it and samples it on the canonical arc grid booleans use (so an
//! untouched face and a boolean imprint of the same circle agree too). Both
//! faces call
//! [`loop_points`], so they insert the identical points (computed in one
//! canonical direction, reversed for the twin) and stay watertight by
//! construction. Where the curve cannot be decided — two planes, a pair of
//! surfaces with no shared circle, an arc whose endpoints don't say which
//! way round — the edge stays straight, exactly as before. An edge already
//! sampled at grid density gets nothing inserted, so dense (frozen)
//! boundaries come through unchanged.

use std::cell::Cell;
use std::f64::consts::PI;

use vcad_kernel_geom::{
    ConeSurface, CylinderSurface, GeometryStore, Plane, SphereSurface, Surface,
};
use vcad_kernel_math::{Point3, Vec3};
use vcad_kernel_topo::{HalfEdgeId, LoopId, Topology};

use crate::TessellationParams;

/// Distance (mm) within which an endpoint counts as lying on a candidate
/// circle. Far below any feature, far above f64 construction noise.
const ON_CURVE_TOL: f64 = 1e-6;

/// Widest arc (radians) a single edge may span and still be resampled. An
/// edge's two endpoints only say which way round the arc goes when it is
/// the short way; a half circle or more stays straight (fail closed) — the
/// paths that build full rings already handle those edges themselves.
const MAX_EDGE_ARC: f64 = 0.9 * PI;

/// The loop's boundary points: each half-edge's origin followed by the
/// interior samples of its edge, in loop order.
pub(crate) fn loop_points(
    topo: &Topology,
    geom: &GeometryStore,
    loop_id: LoopId,
    params: &TessellationParams,
) -> Vec<Point3> {
    let mut out = Vec::new();
    for he in topo.loop_half_edges(loop_id) {
        out.push(topo.vertices[topo.half_edges[he].origin].point);
        out.extend(edge_interior_samples(topo, geom, he, params));
    }
    out
}

/// A circle in 3-D: centre and radius (its plane is implied by the two
/// endpoints it is sampled between).
#[derive(Clone, Copy)]
struct Circle {
    center: Point3,
    radius: f64,
}

/// Surfaces of revolution reduce to an axis plus a radius profile; that is
/// all circle inference needs.
#[derive(Clone, Copy)]
enum Carrier {
    Plane {
        origin: Point3,
        normal: Vec3,
    },
    /// Axis through `origin` along unit `dir`. `sphere` marks a sphere, whose
    /// axis is free: any line through its centre.
    Revolution {
        origin: Point3,
        dir: Vec3,
        sphere: Option<f64>,
    },
}

fn carrier(surface: &dyn Surface) -> Option<Carrier> {
    let any = surface.as_any();
    if let Some(p) = any.downcast_ref::<Plane>() {
        return Some(Carrier::Plane {
            origin: p.origin,
            normal: *p.normal_dir.as_ref(),
        });
    }
    if let Some(c) = any.downcast_ref::<CylinderSurface>() {
        return Some(Carrier::Revolution {
            origin: c.center,
            dir: *c.axis.as_ref(),
            sphere: None,
        });
    }
    if let Some(c) = any.downcast_ref::<ConeSurface>() {
        return Some(Carrier::Revolution {
            origin: c.apex,
            dir: *c.axis.as_ref(),
            sphere: None,
        });
    }
    // Tori are deliberately absent: the torus paths build their own rings
    // (two-chain blends, full tubes) and do not read `loop_points`, so a
    // neighbour that threaded samples into a shared edge would open it.
    // An edge on a torus therefore stays as its chain.
    if let Some(s) = any.downcast_ref::<SphereSurface>() {
        return Some(Carrier::Revolution {
            origin: s.center,
            dir: *s.axis.as_ref(),
            sphere: Some(s.radius.abs()),
        });
    }
    None
}

/// Circle about axis (`origin`, unit `dir`) through `a`, if `b` lies on it.
fn axial_circle(origin: Point3, dir: Vec3, a: Point3, b: Point3) -> Option<Circle> {
    let ha = (a - origin).dot(dir);
    let hb = (b - origin).dot(dir);
    if (ha - hb).abs() > ON_CURVE_TOL {
        return None;
    }
    let center = origin + dir * ha;
    let (ra, rb) = ((a - center).norm(), (b - center).norm());
    if (ra - rb).abs() > ON_CURVE_TOL || ra < 1e-9 {
        return None;
    }
    Some(Circle { center, radius: ra })
}

/// The circle two surfaces share along the edge `a`–`b`, if there is one.
fn shared_circle(c1: Carrier, c2: Carrier, a: Point3, b: Point3) -> Option<Circle> {
    use Carrier::*;
    match (c1, c2) {
        (Plane { .. }, Plane { .. }) => None,
        (
            Plane { origin, normal },
            Revolution {
                origin: o,
                dir,
                sphere,
            },
        )
        | (
            Revolution {
                origin: o,
                dir,
                sphere,
            },
            Plane { origin, normal },
        ) => {
            // Both endpoints must lie in the plane.
            if (a - origin).dot(normal).abs() > ON_CURVE_TOL
                || (b - origin).dot(normal).abs() > ON_CURVE_TOL
            {
                return None;
            }
            if sphere.is_some() {
                // Plane ∩ sphere: a circle about the plane-normal axis
                // through the sphere's centre, at any plane orientation.
                return axial_circle(o, normal, a, b);
            }
            // Plane ∩ surface of revolution is a circle only when the plane
            // is perpendicular to the axis.
            if normal.cross(dir).norm() > 1e-9 {
                return None;
            }
            axial_circle(o, dir, a, b)
        }
        (
            Revolution {
                origin: o1,
                dir: d1,
                sphere: s1,
            },
            Revolution {
                origin: o2,
                dir: d2,
                sphere: s2,
            },
        ) => {
            // Coaxial surfaces of revolution meet in circles about the
            // common axis. A sphere is coaxial with anything whose axis
            // passes through its centre.
            let on_axis = |p: Point3, o: Point3, d: Vec3| {
                let v = p - o;
                (v - d * v.dot(d)).norm() <= ON_CURVE_TOL
            };
            let (origin, dir) = match (s1, s2) {
                (Some(_), Some(_)) => {
                    // Two spheres: circle about the line of centres.
                    let d = o2 - o1;
                    let l = d.norm();
                    if l < 1e-9 {
                        return None;
                    }
                    (o1, d / l)
                }
                (Some(_), None) => {
                    if !on_axis(o1, o2, d2) {
                        return None;
                    }
                    (o2, d2)
                }
                (None, Some(_)) => {
                    if !on_axis(o2, o1, d1) {
                        return None;
                    }
                    (o1, d1)
                }
                (None, None) => {
                    if d1.cross(d2).norm() > 1e-9 || !on_axis(o2, o1, d1) {
                        return None;
                    }
                    (o1, d1)
                }
            };
            axial_circle(origin, dir, a, b)
        }
    }
}

/// Interior samples of half-edge `he`, from its origin toward its
/// destination (endpoints excluded). Empty when the edge is straight, its
/// curve is undecidable, or it is already as dense as the target spacing.
pub(crate) fn edge_interior_samples(
    topo: &Topology,
    geom: &GeometryStore,
    he: HalfEdgeId,
    params: &TessellationParams,
) -> Vec<Point3> {
    // Every lookup is fallible: the boolean pipeline tessellates its
    // operands between splitting and sewing, when a twin can still name a
    // half-edge, loop or face the split has since removed. Such an edge
    // simply stays straight.
    let Some(h) = topo.half_edges.get(he) else {
        return Vec::new();
    };
    let Some(twin) = h.twin else {
        return Vec::new();
    };
    let Some(t) = topo.half_edges.get(twin) else {
        return Vec::new();
    };
    let face_of = |h: &vcad_kernel_topo::HalfEdge| {
        let l = topo.loops.get(h.loop_id?)?;
        let f = l.face?;
        topo.faces.get(f).map(|face| (f, face.surface_index))
    };
    let (Some((f1, i1)), Some((f2, i2))) = (face_of(h), face_of(t)) else {
        return Vec::new();
    };
    let (va, vb) = (h.origin, t.origin);
    if va == vb {
        return Vec::new();
    }
    // Canonical direction: always sample from the lower vertex key, so the
    // two faces sharing this edge compute bit-identical points.
    let (lo, hi, flip) = if va < vb {
        (va, vb, false)
    } else {
        (vb, va, true)
    };
    let (Some(a), Some(b)) = (topo.vertices.get(lo), topo.vertices.get(hi)) else {
        return Vec::new();
    };
    let (a, b) = (a.point, b.point);
    // Order the face pair canonically too: the carrier match is symmetric,
    // but the float path through it should not depend on which side asks.
    let (ia, ib) = if f1 < f2 { (i1, i2) } else { (i2, i1) };
    let (Some(s1), Some(s2)) = (geom.surfaces.get(ia), geom.surfaces.get(ib)) else {
        return Vec::new();
    };
    let (s1, s2) = (s1.as_ref(), s2.as_ref());
    let (Some(c1), Some(c2)) = (carrier(s1), carrier(s2)) else {
        return Vec::new();
    };
    let Some(circle) = shared_circle(c1, c2, a, b) else {
        return Vec::new();
    };
    let mut pts = sample_arc(circle, a, b, params);
    if flip {
        pts.reverse();
    }
    pts
}

/// Points strictly between `a` and `b` on the short arc of `circle`, on the
/// canonical arc grid ([`crate::canonical_arc`]) — the grid booleans freeze
/// split rims onto. Sampling arcs there is what lets an untouched extrude
/// wall meet a boolean's imprint of the same circle on its neighbour (the
/// K1 wrist flange ∪ D union: a sparse wall rail against a dense imprinted
/// hole, 202 open edges) with identical points.
///
/// An edge spanning under [`MIN_GRID_SPAN`] grid steps is left alone: it is
/// already sampled at grid density (a frozen rim, an SSI polyline), and
/// threading the odd grid point into it would only add slivers.
fn sample_arc(circle: Circle, a: Point3, b: Point3, params: &TessellationParams) -> Vec<Point3> {
    let u = a - circle.center;
    let vb = b - circle.center;
    let theta = (vb.dot(u) / (circle.radius * circle.radius))
        .clamp(-1.0, 1.0)
        .acos();
    if !(1e-9..=MAX_EDGE_ARC).contains(&theta) {
        return Vec::new();
    }
    if UNIFORM_ARC_GRID.with(Cell::get) {
        return sample_arc_uniform(circle, u, vb, theta, params);
    }
    // Travel the short way: counterclockwise about u × vb.
    let normal = u.cross(vb);
    if normal.norm() < 1e-12 {
        return Vec::new();
    }
    let segments = params.circle_segments_for_radius(circle.radius);
    let n = crate::canonical_arc::arc_segments(circle.radius, segments);
    if theta < MIN_GRID_SPAN * 2.0 * PI / n as f64 {
        return Vec::new();
    }
    let mut pts = crate::canonical_arc::canonical_arc_points(
        circle.center,
        circle.radius,
        normal,
        a,
        b,
        segments,
    );
    // [start, interior…, end] → interior only.
    pts.pop();
    if !pts.is_empty() {
        pts.remove(0);
    }
    pts
}

/// Grid steps an edge must span before arc samples are threaded into it.
const MIN_GRID_SPAN: f64 = 1.5;

thread_local! {
    /// Set for the duration of [`with_uniform_arc_grid`].
    static UNIFORM_ARC_GRID: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` with arc edges sampled on a uniform per-edge subdivision at the
/// tessellation step instead of the canonical grid.
///
/// The frozen tessellation of the differentiable seam needs this: its FD
/// oracles re-capture a plan at θ ± h, and the canonical grid's count and
/// angles move with the radius, so the two sides of a stencil would carry
/// different discretizations. A uniform split of each edge into
/// `ceil(arc / step)` spans keeps the count fixed while the geometry moves.
/// Scoped to the calling thread and restored on exit (including unwind).
pub(crate) fn with_uniform_arc_grid<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            UNIFORM_ARC_GRID.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(UNIFORM_ARC_GRID.with(|c| c.replace(true)));
    f()
}

/// Uniform subdivision of an arc of `theta` from `u` toward `vb` (both
/// relative to the centre) into spans no wider than the tessellation step.
fn sample_arc_uniform(
    circle: Circle,
    u: Vec3,
    vb: Vec3,
    theta: f64,
    params: &TessellationParams,
) -> Vec<Point3> {
    let w_raw = vb - u * (vb.dot(u) / u.dot(u));
    let w_len = w_raw.norm();
    if w_len < 1e-12 {
        return Vec::new();
    }
    let w = w_raw * (circle.radius / w_len);
    let n = params.circle_segments_for_radius(circle.radius).max(3) as f64;
    let k = (theta / (2.0 * PI / n) - 1e-9).ceil() as usize;
    if k <= 1 {
        return Vec::new();
    }
    (1..k)
        .map(|i| {
            let t = theta * i as f64 / k as f64;
            circle.center + u * t.cos() + w * t.sin()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn circle_z(r: f64) -> Circle {
        Circle {
            center: Point3::origin(),
            radius: r,
        }
    }

    #[test]
    fn short_arc_takes_the_canonical_grid() {
        let r = 10.0;
        let a = Point3::new(r, 0.0, 0.0);
        let t = PI / 4.0;
        let b = Point3::new(r * t.cos(), r * t.sin(), 0.0);
        let p = TessellationParams::from_segments(64);
        let pts = sample_arc(circle_z(r), a, b, &p);
        let n = crate::canonical_arc::arc_segments(r, 64) as f64;
        let step = 2.0 * PI / n;
        // Every grid angle strictly inside (0, 45°).
        assert_eq!(pts.len(), ((t / step) - 1e-9).floor() as usize);
        let mut last = 0.0;
        for q in &pts {
            assert!(((q - Point3::origin()).norm() - r).abs() < 1e-9);
            let ang = q.y.atan2(q.x);
            assert!(ang > last && ang < t, "monotonic inside the arc");
            assert!(
                ((ang / step) - (ang / step).round()).abs() < 1e-6,
                "on the grid"
            );
            last = ang;
        }
        // The twin direction yields the same points, reversed.
        let mut back = sample_arc(circle_z(r), b, a, &p);
        back.reverse();
        assert_eq!(back.len(), pts.len());
        for (x, y) in back.iter().zip(&pts) {
            assert!((x - y).norm() < 1e-12);
        }
    }

    #[test]
    fn already_dense_edge_gets_nothing() {
        let r = 10.0;
        let n = crate::canonical_arc::arc_segments(r, 64) as f64;
        let a = Point3::new(r, 0.0, 0.0);
        let t = 2.0 * PI / n;
        let b = Point3::new(r * t.cos(), r * t.sin(), 0.0);
        let p = TessellationParams::from_segments(64);
        assert!(sample_arc(circle_z(r), a, b, &p).is_empty());
    }

    #[test]
    fn half_circle_edge_fails_closed() {
        let r = 10.0;
        let p = TessellationParams::from_segments(64);
        let pts = sample_arc(
            circle_z(r),
            Point3::new(r, 0.0, 0.0),
            Point3::new(-r, 1e-3, 0.0),
            &p,
        );
        assert!(pts.is_empty());
    }
}
