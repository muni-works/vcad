//! Analytic correction of tessellation-based clearance for cylinder pairs.
//!
//! [`vcad_kernel_tessellate::mesh_clearance`] measures polygons, not
//! surfaces: a cylinder of radius `r` meshed at `n` segments has chords up
//! to `r·(1 − cos(π/n))` inside its true surface. A 7.98 mm-radius shaft in
//! an 8.00 mm bore (true radial gap +0.02 mm) therefore meshes as an
//! interference of ≈ −0.018 mm — the bore's chords cut into the shaft — and a
//! gap smaller than the chord sag reads as a collision.
//!
//! For two BRep solids this module recognises when the mesh's witness pair
//! sits on two *parallel* cylinders (shaft in bore — coaxial or offset — or
//! two shafts side by side) and replaces the polygon number with the closed
//! form:
//!
//! - shaft (outward) in bore (inward), axes offset `e`: `r_bore − r_shaft − e`
//! - two shafts, axes `e` apart: `e − r₁ − r₂`
//!
//! Fail-closed rules — the analytic number is used only when it provably
//! explains the mesh result:
//! 1. both witness points lie within the chord-sag band of their cylinder
//!    (radially, and axially inside the faces' common span);
//! 2. the mesh distance is within the combined sag band of the closed form
//!    (otherwise something else produced the mesh number);
//! 3. with those cylinder faces left out, the remaining surfaces are no
//!    closer than the closed form — so a shoulder, key or flange that
//!    really is nearer (or crossing) still wins.
//!
//! A negative closed form stays an interference (`intersecting`), with the
//! exact radial depth.

use vcad_kernel_geom::CylinderSurface;
use vcad_kernel_primitives::BRepSolid;
use vcad_kernel_tessellate::{
    mesh_surface_distance, tessellate_brep, ClearanceResult, TriangleMesh,
};
use vcad_kernel_topo::Orientation;

type V3 = [f64; 3];

/// Axes closer than this to parallel (radians) count as parallel.
const PARALLEL_RAD: f64 = 1e-7;
/// Cylinders are "the same surface" within this, mm.
const SAME_SURFACE_MM: f64 = 1e-6;
/// Safety factor on the theoretical chord sag.
const SAG_SLACK: f64 = 1.5;
/// Absolute floor on every band, mm (f32 mesh coordinates).
const BAND_FLOOR_MM: f64 = 1e-4;

/// How a clearance number was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearanceBasis {
    /// Tessellated mesh distance (carries chord error).
    Mesh,
    /// Closed form between two parallel cylinders.
    AnalyticCylinders,
}

impl ClearanceBasis {
    /// Stable wire name (`"mesh"` / `"analytic_cylinders"`).
    pub fn as_str(self) -> &'static str {
        match self {
            ClearanceBasis::Mesh => "mesh",
            ClearanceBasis::AnalyticCylinders => "analytic_cylinders",
        }
    }
}

/// A clearance result plus how it was obtained.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RefinedClearance {
    /// The (possibly corrected) result.
    pub result: ClearanceResult,
    /// Mesh or analytic.
    pub basis: ClearanceBasis,
    /// The mesh distance before correction, mm.
    pub mesh_distance: f64,
    /// Combined chord-sag band of the cylinder pair, mm (analytic only):
    /// how far the mesh number could stray from the true surfaces.
    pub chord_band_mm: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct Cyl {
    /// Ordinal of the face in the solid's outer shell (= mesh `face_ids`).
    ordinal: u32,
    origin: V3,
    axis: V3,
    radius: f64,
    inward: bool,
    /// Axial span of the face's loop vertices, in its own frame.
    z: (f64, f64),
}

fn cylinders(brep: &BRepSolid) -> Vec<Cyl> {
    let topo = &brep.topology;
    let solid = &topo.solids[brep.solid_id];
    let shell = &topo.shells[solid.outer_shell];
    let mut out = Vec::new();
    for (ordinal, &fid) in shell.faces.iter().enumerate() {
        let f = &topo.faces[fid];
        let s = brep.geometry.surfaces[f.surface_index].as_ref();
        let Some(c) = s.as_any().downcast_ref::<CylinderSurface>() else {
            continue;
        };
        let o = [c.center.x, c.center.y, c.center.z];
        let av = c.axis.as_vec();
        let a = [av.x, av.y, av.z];
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for lp in std::iter::once(f.outer_loop).chain(f.inner_loops.iter().copied()) {
            for v in topo.loop_vertices(lp) {
                let p = topo.vertices[v].point;
                let z = dot(sub([p.x, p.y, p.z], o), a);
                lo = lo.min(z);
                hi = hi.max(z);
            }
        }
        if !lo.is_finite() {
            continue;
        }
        out.push(Cyl {
            ordinal: ordinal as u32,
            origin: o,
            axis: a,
            radius: c.radius,
            inward: f.orientation == Orientation::Reversed,
            z: (lo, hi),
        });
    }
    out
}

fn sag(radius: f64, segments: u32) -> f64 {
    radius * (1.0 - (std::f64::consts::PI / f64::from(segments.max(3))).cos())
}

/// Radial distance of `p` from the cylinder's axis, and its axial coordinate.
fn radial_axial(c: &Cyl, p: V3) -> (f64, f64) {
    let d = sub(p, c.origin);
    let t = dot(d, c.axis);
    (norm(sub(d, scale(c.axis, t))), t)
}

/// `p` projected radially onto the cylinder surface.
fn project(c: &Cyl, p: V3) -> V3 {
    let d = sub(p, c.origin);
    let t = dot(d, c.axis);
    let radial = sub(d, scale(c.axis, t));
    let n = norm(radial);
    if n < 1e-12 {
        return p;
    }
    add(add(c.origin, scale(c.axis, t)), scale(radial, c.radius / n))
}

/// Closed-form signed gap between two parallel cylinders, or `None` for a
/// configuration without one (two bores, shaft axis outside the bore).
fn closed_form(x: &Cyl, y: &Cyl) -> Option<f64> {
    let e = radial_axial(y, x.origin).0;
    match (x.inward, y.inward) {
        (false, false) => Some(e - x.radius - y.radius),
        (false, true) if e < y.radius => Some(y.radius - x.radius - e),
        (true, false) if e < x.radius => Some(x.radius - y.radius - e),
        _ => None,
    }
}

struct Match {
    gap: f64,
    band: f64,
    x: Cyl,
    y: Cyl,
}

/// Find the parallel-cylinder pair that explains `mesh`'s witness points.
fn explaining_pair(
    ca: &[Cyl],
    cb: &[Cyl],
    seg_a: u32,
    seg_b: u32,
    mesh: &ClearanceResult,
) -> Option<Match> {
    let mut best: Option<Match> = None;
    for x in ca {
        let sx = SAG_SLACK * sag(x.radius, seg_a) + BAND_FLOOR_MM;
        let (rx, tx) = radial_axial(x, mesh.point_a);
        if (rx - x.radius).abs() > sx || tx < x.z.0 - sx || tx > x.z.1 + sx {
            continue;
        }
        for y in cb {
            if line_angle(x.axis, y.axis) > PARALLEL_RAD {
                continue;
            }
            let sy = SAG_SLACK * sag(y.radius, seg_b) + BAND_FLOOR_MM;
            let (ry, ty) = radial_axial(y, mesh.point_b);
            if (ry - y.radius).abs() > sy || ty < y.z.0 - sy || ty > y.z.1 + sy {
                continue;
            }
            // Common axial span (y's frame) must be real, not an edge touch.
            let shift = dot(sub(x.origin, y.origin), y.axis);
            let (x0, x1) = {
                let s = dot(x.axis, y.axis).signum();
                let (a, b) = (x.z.0 * s + shift, x.z.1 * s + shift);
                (a.min(b), a.max(b))
            };
            if x1.min(y.z.1) - x0.max(y.z.0) <= SAME_SURFACE_MM {
                continue;
            }
            let Some(gap) = closed_form(x, y) else {
                continue;
            };
            let band = sx + sy;
            if (gap - mesh.distance).abs() > band {
                continue;
            }
            if best.as_ref().is_none_or(|m| gap < m.gap) {
                best = Some(Match {
                    gap,
                    band,
                    x: *x,
                    y: *y,
                });
            }
        }
    }
    best
}

fn same_surface(a: &Cyl, b: &Cyl) -> bool {
    a.inward == b.inward
        && line_angle(a.axis, b.axis) <= PARALLEL_RAD
        && (a.radius - b.radius).abs() <= SAME_SURFACE_MM
        && radial_axial(a, b.origin).0 <= SAME_SURFACE_MM
}

/// `brep`'s mesh at `segments` with every face on `cyl`'s surface removed.
fn mesh_without(brep: &BRepSolid, segments: u32, cyls: &[Cyl], cyl: &Cyl) -> TriangleMesh {
    let drop: Vec<u32> = cyls
        .iter()
        .filter(|c| same_surface(c, cyl))
        .map(|c| c.ordinal)
        .collect();
    let full = tessellate_brep(brep, segments);
    let mut out = TriangleMesh::new();
    out.vertices = full.vertices.clone();
    let tagged = full.face_ids.len() * 3 == full.indices.len();
    for (t, tri) in full.indices.chunks_exact(3).enumerate() {
        if tagged && drop.contains(&full.face_ids[t]) {
            continue;
        }
        out.indices.extend_from_slice(tri);
    }
    out
}

/// Correct a mesh clearance between two BRep solids with the parallel
/// cylinder closed form, when (and only when) that pair provably produced
/// the mesh number. See the module docs for the rules. `segments_*` are the
/// densities the meshes were (at most as coarse as) tessellated at; they
/// set the chord-sag bands.
pub fn refine_cylinder_clearance(
    a: &BRepSolid,
    segments_a: u32,
    b: &BRepSolid,
    segments_b: u32,
    mesh: &ClearanceResult,
) -> RefinedClearance {
    let unrefined = RefinedClearance {
        result: *mesh,
        basis: ClearanceBasis::Mesh,
        mesh_distance: mesh.distance,
        chord_band_mm: None,
    };
    let (ca, cb) = (cylinders(a), cylinders(b));
    if ca.is_empty() || cb.is_empty() {
        return unrefined;
    }
    let Some(m) = explaining_pair(&ca, &cb, segments_a, segments_b, mesh) else {
        return unrefined;
    };
    // Rule 3: the rest of the geometry must not be nearer than the pair.
    let rest_a = mesh_without(a, segments_a, &ca, &m.x);
    let rest_b = mesh_without(b, segments_b, &cb, &m.y);
    if let Some(r) = mesh_surface_distance(&rest_a, &rest_b) {
        // A remaining witness on the same two cylinders' rims is the same
        // feature seen through its neighbours' chords, not a rival.
        let on_pair = {
            let (rx, _) = radial_axial(&m.x, r.point_a);
            let (ry, _) = radial_axial(&m.y, r.point_b);
            (rx - m.x.radius).abs() <= m.band && (ry - m.y.radius).abs() <= m.band
        };
        if !on_pair && (r.intersecting || r.distance < m.gap) {
            return unrefined;
        }
    }
    RefinedClearance {
        result: ClearanceResult {
            distance: m.gap,
            intersecting: m.gap < 0.0,
            point_a: project(&m.x, mesh.point_a),
            point_b: project(&m.y, mesh.point_b),
        },
        basis: ClearanceBasis::AnalyticCylinders,
        mesh_distance: mesh.distance,
        chord_band_mm: Some(m.band),
    }
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}
fn line_angle(a: V3, b: V3) -> f64 {
    norm(cross(a, b)).atan2(dot(a, b).abs())
}

#[cfg(test)]
mod tests {
    use crate::Solid;

    /// `r_shaft` shaft (height 30, z −5..25) through a 20 mm block with an
    /// `r_bore` through bore, both on the Z axis.
    fn shaft_in_bore(r_shaft: f64, r_bore: f64) -> (Solid, Solid) {
        let block = Solid::cube(20.0, 20.0, 20.0)
            .translate(-10.0, -10.0, 0.0)
            .difference(&Solid::cylinder(r_bore, 40.0, 32).translate(0.0, 0.0, -10.0));
        let shaft = Solid::cylinder(r_shaft, 30.0, 32).translate(0.0, 0.0, -5.0);
        (shaft, block)
    }

    #[test]
    fn coaxial_shaft_in_bore_reports_the_true_radial_gap() {
        let (shaft, block) = shaft_in_bore(7.98, 8.0);
        let r = shaft.clearance_refined(&block).expect("measurable");
        assert_eq!(r.basis, super::ClearanceBasis::AnalyticCylinders, "{r:?}");
        assert!(!r.result.intersecting, "{r:?}");
        assert!((r.result.distance - 0.02).abs() < 1e-9, "{r:?}");
        // And the plain API agrees.
        let c = shaft.clearance(&block).unwrap();
        assert!(!c.intersecting && (c.distance - 0.02).abs() < 1e-9, "{c:?}");
    }

    #[test]
    fn oversized_shaft_still_interferes_with_exact_depth() {
        let (shaft, block) = shaft_in_bore(8.02, 8.0);
        let c = shaft.clearance(&block).unwrap();
        assert!(c.intersecting, "{c:?}");
        assert!((c.distance + 0.02).abs() < 1e-9, "{c:?}");
    }

    /// A mesh result whose witness sits on the shaft/bore pair (what a
    /// coarse bore tessellation produces): shaft point at r 7.98, bore point
    /// at the chord's inscribed radius, reading a −0.018 "interference".
    fn chord_noise_witness() -> vcad_kernel_tessellate::ClearanceResult {
        vcad_kernel_tessellate::ClearanceResult {
            distance: -0.018,
            intersecting: true,
            point_a: [-7.98, 0.0, 10.0],
            point_b: [-7.962, 0.0, 10.0],
        }
    }

    #[test]
    fn chord_noise_on_the_pair_is_replaced_by_the_closed_form() {
        let (shaft, block) = shaft_in_bore(7.98, 8.0);
        let r = shaft.refine_clearance(&block, &chord_noise_witness());
        assert_eq!(r.basis, super::ClearanceBasis::AnalyticCylinders, "{r:?}");
        assert!(!r.result.intersecting, "{r:?}");
        assert!((r.result.distance - 0.02).abs() < 1e-9, "{r:?}");
        assert!((r.mesh_distance + 0.018).abs() < 1e-12, "{r:?}");
    }

    #[test]
    fn a_real_shoulder_interference_is_not_hidden() {
        // Same witness on the shaft/bore pair, but the shaft now carries a
        // 10 mm-radius collar sunk 0.005 mm into the block's top face — a
        // genuine interference shallower than the chord noise. Rule 3 (the
        // rest of the geometry must not be nearer) keeps the mesh verdict.
        let (shaft, block) = shaft_in_bore(7.98, 8.0);
        let collar = Solid::cylinder(10.0, 3.0, 32).translate(0.0, 0.0, 19.995);
        let shaft = shaft.union(&collar);
        let r = shaft.refine_clearance(&block, &chord_noise_witness());
        assert_eq!(r.basis, super::ClearanceBasis::Mesh, "{r:?}");
        assert!(r.result.intersecting, "{r:?}");
        // End to end as well: the collar is what the real measurement sees.
        let c = shaft.clearance(&block).unwrap();
        assert!(c.intersecting && c.distance < 0.0, "{c:?}");
    }

    #[test]
    fn a_witness_off_the_cylinders_is_left_alone() {
        // Mesh says the parts are 0.019 apart at a point that is not on the
        // shaft surface: nothing to explain, no correction.
        let (shaft, block) = shaft_in_bore(7.98, 8.0);
        let mut m = chord_noise_witness();
        m.point_a = [-7.5, 0.0, 10.0];
        let r = shaft.refine_clearance(&block, &m);
        assert_eq!(r.basis, super::ClearanceBasis::Mesh, "{r:?}");
    }

    #[test]
    fn side_by_side_pins_use_the_closed_form() {
        let a = Solid::cylinder(3.0, 10.0, 32);
        let b = Solid::cylinder(3.0, 10.0, 32).translate(6.01, 0.0, 0.0);
        let c = a.clearance(&b).unwrap();
        assert!(!c.intersecting, "{c:?}");
        assert!((c.distance - 0.01).abs() < 1e-9, "{c:?}");
    }
}
