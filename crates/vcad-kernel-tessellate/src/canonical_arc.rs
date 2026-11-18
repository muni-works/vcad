//! The canonical arc grid: the one discretization of a circular arc that
//! booleans and the tessellator share.
//!
//! Booleans freeze split rims onto this grid; the tessellator samples every
//! arc edge from it (see `edge_samples`). Two faces that meet along a
//! circle therefore carry identical points however they came to border it,
//! a boolean imprint on one side and an untouched extrude wall on the
//! other included. Moved here from `vcad-kernel-booleans` (which depends on
//! this crate) so both read one implementation.

use std::f64::consts::PI;

use vcad_kernel_math::Point3;

fn snap_coord(v: f64) -> f64 {
    if v.abs() < 1e-6 {
        0.0
    } else {
        v
    }
}

/// Snap a point's coordinates to 0 if they're very close to 0.
fn snap_point(p: Point3) -> Point3 {
    Point3::new(snap_coord(p.x), snap_coord(p.y), snap_coord(p.z))
}

/// Segments per full circle on the canonical rim grid:
/// `max(requested, sag_count)` at a 1e-3 mm sag, clamped to 3..=512.
pub fn arc_segments(radius: f64, segments: u32) -> u32 {
    // Must match ssi::ellipse_samples' sag: SSI polylines and canonical
    // rings share vertices only when both discretize at the same density.
    const SAG: f64 = 1e-3;
    let n = if radius > SAG {
        let arg = (1.0 - SAG / radius).clamp(-1.0, 1.0);
        (std::f64::consts::PI / arg.acos()).ceil() as u32
    } else {
        3
    };
    n.max(segments).clamp(3, 512)
}

/// Canonical full-circle polyline: every grid point of the sag-dense
/// canonical frame, CCW about `normal`. Any two faces sampling the same
/// circle (a frozen wall ring, a cap hole boundary) get identical points.
pub fn canonical_circle_points(
    center: Point3,
    radius: f64,
    normal: vcad_kernel_math::Vec3,
    segments: u32,
) -> Vec<Point3> {
    // Reuse the arc sampler with start == end: it returns
    // [start, interior grid..., start]; drop the duplicated closing point.
    let x_axis = {
        // Same canonical-frame derivation as canonical_arc_points.
        let mut n_hat = normal.normalize();
        for c in [n_hat.x, n_hat.y, n_hat.z] {
            if c.abs() > 1e-9 {
                if c < 0.0 {
                    n_hat = -n_hat;
                }
                break;
            }
        }
        let cand = [
            vcad_kernel_math::Vec3::new(1.0, 0.0, 0.0),
            vcad_kernel_math::Vec3::new(0.0, 1.0, 0.0),
            vcad_kernel_math::Vec3::new(0.0, 0.0, 1.0),
        ];
        let e = cand
            .into_iter()
            .min_by(|a, b| a.dot(n_hat).abs().partial_cmp(&b.dot(n_hat).abs()).unwrap())
            .unwrap();
        (e - n_hat * e.dot(n_hat)).normalize()
    };
    let seam = center + radius * x_axis;
    let mut ring = canonical_arc_points(center, radius, normal, seam, seam, segments);
    ring.pop();
    ring
}

/// Canonical arc samples `[start, interior…, end]`, travelling
/// counterclockwise about `normal`. Interior points are the grid angles of a
/// frame fixed by the circle alone (centre, radius, sign-canonical normal),
/// so every face sharing an arc reproduces bit-identical interior points
/// regardless of its own parameterization.
pub fn canonical_arc_points(
    center: Point3,
    radius: f64,
    normal: vcad_kernel_math::Vec3,
    start: Point3,
    end: Point3,
    segments: u32,
) -> Vec<Point3> {
    let mut n_hat = normal.normalize();
    // Sign-canonicalize the axis so the grid is invariant under normal flip.
    let mut flipped = false;
    for c in [n_hat.x, n_hat.y, n_hat.z] {
        if c.abs() > 1e-9 {
            if c < 0.0 {
                n_hat = -n_hat;
                flipped = true;
            }
            break;
        }
    }
    // Canonical in-plane frame: global axis least parallel to n̂.
    let cand = [
        vcad_kernel_math::Vec3::new(1.0, 0.0, 0.0),
        vcad_kernel_math::Vec3::new(0.0, 1.0, 0.0),
        vcad_kernel_math::Vec3::new(0.0, 0.0, 1.0),
    ];
    let e = cand
        .into_iter()
        .min_by(|a, b| a.dot(n_hat).abs().partial_cmp(&b.dot(n_hat).abs()).unwrap())
        .unwrap();
    let x_axis = (e - n_hat * e.dot(n_hat)).normalize();
    let y_axis = n_hat.cross(x_axis);

    let angle_of = |p: Point3| -> f64 {
        let d = p - center;
        let a = d.dot(y_axis).atan2(d.dot(x_axis));
        if a < 0.0 {
            a + 2.0 * PI
        } else {
            a
        }
    };
    let a_start = angle_of(start);
    let a_end = angle_of(end);
    // CCW about the ORIGINAL normal; in the canonical frame that is CCW
    // unless the axis was flipped.
    let ccw = !flipped;
    let span = if ccw {
        (a_end - a_start).rem_euclid(2.0 * PI)
    } else {
        (a_start - a_end).rem_euclid(2.0 * PI)
    };
    let span = if span < 1e-12 { 2.0 * PI } else { span };

    let n = arc_segments(radius, segments);
    let step = 2.0 * PI / n as f64;
    let mut pts = vec![start];
    // Interior grid angles strictly inside the traversal (ε away from the
    // endpoints so a grid point coincident with an endpoint isn't doubled).
    let eps = 1e-9;
    // Walk grid indices in traversal order: find the first grid angle
    // strictly after a_start (in travel direction), then step until a_end.
    let first_idx = if ccw {
        (a_start / step).floor() + 1.0
    } else {
        (a_start / step).ceil() - 1.0
    };
    // `traveled` must accumulate monotonically — computing it per-index with
    // rem_euclid(2π) wraps back below `span` after one full revolution, so a
    // full-circle arc whose seam is off the grid re-emits the first grid
    // points as duplicates (and only the safety cap stopped it).
    let mut idx = first_idx;
    let mut traveled = {
        let ang = idx * step;
        if ccw {
            (ang - a_start).rem_euclid(2.0 * PI)
        } else {
            (a_start - ang).rem_euclid(2.0 * PI)
        }
    };
    while traveled < span - eps {
        if traveled > eps {
            let a = (idx * step).rem_euclid(2.0 * PI);
            let (sin_a, cos_a) = a.sin_cos();
            pts.push(snap_point(
                center + radius * (cos_a * x_axis + sin_a * y_axis),
            ));
        }
        if ccw {
            idx += 1.0;
        } else {
            idx -= 1.0;
        }
        traveled += step;
    }
    pts.push(end);
    pts
}
