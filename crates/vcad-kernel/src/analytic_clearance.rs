//! Analytic correction of tessellation-based clearance for cylinder pairs.
//!
//! [`vcad_kernel_tessellate::mesh_clearance`] measures polygons, not
//! surfaces: a cylinder of radius `r` meshed at `n` segments has chords up
//! to `r·(1 − cos(π/n))` inside its true surface, so a running fit tighter
//! than the chord sag can mesh as an interference, and a fit sitting exactly
//! on its required clearance fails by the chord error.
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
//! explains the mesh result. Each cylinder's *band* is the measured radial
//! deviation of the kernel's own tessellation of that surface (× 1.5), so a
//! sag-adaptive boolean bore gets a tight band and a 32-gon primitive an
//! honest one; a caller mesh coarser than the kernel's simply never matches.
//! 1. both witness points lie within their cylinder's band (radially, and
//!    axially inside the face's span);
//! 2. the mesh distance is within the two bands of the closed form
//!    (otherwise something else produced the mesh number);
//! 3. nothing else is nearer:
//!    a. with those cylinder surfaces left out, the remaining surfaces are
//!       neither crossing nor closer than the closed form (a collar sunk into
//!       the block's face, a key, a flange);
//!    b. where one cylinder runs past the other's end, its surface just past
//!       that end is outside the other solid (a shaft driven through a blind
//!       bore's floor, a shoulder inside the bore) — the cylinder faces
//!       themselves can penetrate, and 3a cannot see that.
//!
//! A negative closed form stays an interference (`intersecting`), with the
//! exact radial depth.

use vcad_kernel_geom::CylinderSurface;
use vcad_kernel_primitives::BRepSolid;
use vcad_kernel_tessellate::{
    mesh_surface_distance, tessellate_brep, ClearanceResult, MeshInsideTester, TriangleMesh,
};
use vcad_kernel_topo::Orientation;

type V3 = [f64; 3];

/// Axes closer than this to parallel (radians) count as parallel.
const PARALLEL_RAD: f64 = 1e-7;
/// Cylinders are "the same surface" within this, mm.
const SAME_SURFACE_MM: f64 = 1e-6;
/// Safety factor on the measured tessellation deviation.
const BAND_SLACK: f64 = 1.5;
/// Absolute floor on every band, mm (f32 mesh coordinates).
const BAND_FLOOR_MM: f64 = 1e-4;
/// Closed forms within this of zero are contact, not interference.
const CONTACT_EPS_MM: f64 = 1e-9;
/// Probe angles around a cylinder for rule 3b.
const PROBE_ANGLES: usize = 16;

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
    /// Combined tessellation band of the cylinder pair, mm (analytic only):
    /// how far the mesh number could stray from the true surfaces.
    pub chord_band_mm: Option<f64>,
}

/// One cylindrical face.
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

/// A cylinder *surface*: every face of a solid lying on one cylinder (a
/// bore is often split into halves), with the union of their axial spans.
#[derive(Debug, Clone)]
struct CylSurface {
    c: Cyl,
    ordinals: Vec<u32>,
    /// Axial span in `c`'s frame.
    span: (f64, f64),
    /// Measured tessellation band, mm.
    band: f64,
}

fn same_surface(a: &Cyl, b: &Cyl) -> bool {
    a.inward == b.inward
        && line_angle(a.axis, b.axis) <= PARALLEL_RAD
        && (a.radius - b.radius).abs() <= SAME_SURFACE_MM
        && radial_axial(a, b.origin).0 <= SAME_SURFACE_MM
}

fn surface_of(cyls: &[Cyl], seed: &Cyl, mesh: &TriangleMesh) -> CylSurface {
    let members: Vec<&Cyl> = cyls.iter().filter(|c| same_surface(c, seed)).collect();
    let mut span = (f64::INFINITY, f64::NEG_INFINITY);
    for m in &members {
        let shift = dot(sub(m.origin, seed.origin), seed.axis);
        let s = dot(m.axis, seed.axis).signum();
        for z in [m.z.0 * s + shift, m.z.1 * s + shift] {
            span.0 = span.0.min(z);
            span.1 = span.1.max(z);
        }
    }
    let ordinals: Vec<u32> = members.iter().map(|m| m.ordinal).collect();
    CylSurface {
        c: *seed,
        band: BAND_SLACK * tessellation_deviation(mesh, &ordinals, seed, span) + BAND_FLOOR_MM,
        ordinals,
        span,
    }
}

/// Largest radial deviation from `c` of the mesh's triangles on `ordinals`,
/// sampled at vertices, edge midpoints and centroids (where chords sag) —
/// plus the rim chords of *neighbouring* faces: an end face's or a
/// channel's edge along the cylinder can be tessellated more coarsely than
/// the cylinder face itself, and a witness lands on it just as often.
fn tessellation_deviation(mesh: &TriangleMesh, ordinals: &[u32], c: &Cyl, span: (f64, f64)) -> f64 {
    if mesh.face_ids.len() * 3 != mesh.indices.len() {
        return f64::INFINITY; // untagged mesh: no way to measure, never match
    }
    let v = |i: u32| {
        let k = 3 * i as usize;
        [
            f64::from(mesh.vertices[k]),
            f64::from(mesh.vertices[k + 1]),
            f64::from(mesh.vertices[k + 2]),
        ]
    };
    // A vertex "on" the cylinder: exact up to f32 storage.
    let rim_tol = 1e-4 + 1e-6 * c.radius;
    // Sag of a chord over 22.5° of arc.
    let max_rim_sag = c.radius * (1.0 - (std::f64::consts::PI / 16.0).cos());
    let on_cyl = |p: V3| {
        let (r, t) = radial_axial(c, p);
        (r - c.radius).abs() <= rim_tol && t >= span.0 - rim_tol && t <= span.1 + rim_tol
    };
    let mut dev: f64 = 0.0;
    for (t, tri) in mesh.indices.as_chunks::<3>().0.iter().enumerate() {
        let (p, q, r) = (v(tri[0]), v(tri[1]), v(tri[2]));
        if !ordinals.contains(&mesh.face_ids[t]) {
            for (a, b) in [(p, q), (q, r), (r, p)] {
                if on_cyl(a) && on_cyl(b) {
                    let d = (radial_axial(c, scale(add(a, b), 0.5)).0 - c.radius).abs();
                    // A rim chord spans a small arc; an edge across a flat
                    // (both ends on the cylinder, midpoint deep inside) is
                    // not a chord of this surface.
                    if d <= max_rim_sag {
                        dev = dev.max(d);
                    }
                }
            }
            continue;
        }
        let samples = [
            p,
            q,
            r,
            scale(add(p, q), 0.5),
            scale(add(q, r), 0.5),
            scale(add(r, p), 0.5),
            scale(add(add(p, q), r), 1.0 / 3.0),
        ];
        for s in samples {
            dev = dev.max((radial_axial(c, s).0 - c.radius).abs());
        }
    }
    dev
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
/// Rounded to the nanometre: `8.0 − 7.98` is `0.019999999999999574` in f64,
/// which would fail a `min_mm: 0.02` assertion the fit meets exactly.
fn closed_form(x: &Cyl, y: &Cyl) -> Option<f64> {
    let e = radial_axial(y, x.origin).0;
    let g = match (x.inward, y.inward) {
        (false, false) => e - x.radius - y.radius,
        (false, true) if e < y.radius => y.radius - x.radius - e,
        (true, false) if e < x.radius => x.radius - y.radius - e,
        _ => return None,
    };
    Some((g * 1e9).round() / 1e9)
}

/// Is the witness `p` on the surface (within its band, inside its span)?
fn on_surface(s: &CylSurface, p: V3, band: f64) -> bool {
    let (r, t) = radial_axial(&s.c, p);
    (r - s.c.radius).abs() <= band && t >= s.span.0 - band && t <= s.span.1 + band
}

/// `s`'s span expressed in `frame`'s axial coordinate.
fn span_in(s: &CylSurface, frame: &Cyl) -> (f64, f64) {
    let shift = dot(sub(s.c.origin, frame.origin), frame.axis);
    let k = dot(s.c.axis, frame.axis).signum();
    let (a, b) = (s.span.0 * k + shift, s.span.1 * k + shift);
    (a.min(b), a.max(b))
}

/// `brep`'s mesh with every triangle on `drop` removed.
fn mesh_without(full: &TriangleMesh, drop: &[u32]) -> TriangleMesh {
    let mut out = TriangleMesh::new();
    out.vertices = full.vertices.clone();
    for (t, tri) in full.indices.as_chunks::<3>().0.iter().enumerate() {
        if !drop.contains(&full.face_ids[t]) {
            out.indices.extend_from_slice(tri);
        }
    }
    out
}

/// Rule 3b: where `s` runs past `other`'s end, is `s`'s surface just past
/// that end inside the solid `inside`?
fn runs_into(s: &CylSurface, other: &CylSurface, inside: &MeshInsideTester) -> bool {
    let (o0, o1) = span_in(other, &s.c);
    let reach = (s.band + other.band).max(1e-3);
    // Two radial frame vectors perpendicular to the axis.
    let a = s.c.axis;
    let helper = if a[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let u = {
        let w = cross(a, helper);
        scale(w, 1.0 / norm(w))
    };
    let v = cross(a, u);
    let probe_at = |t: f64| {
        (0..PROBE_ANGLES).any(|k| {
            let th = std::f64::consts::TAU * k as f64 / PROBE_ANGLES as f64;
            let radial = add(scale(u, th.cos()), scale(v, th.sin()));
            let p = add(add(s.c.origin, scale(a, t)), scale(radial, s.c.radius));
            inside.contains(p)
        })
    };
    let past_hi = s.span.1 - o1;
    let past_lo = o0 - s.span.0;
    (past_hi > SAME_SURFACE_MM && probe_at(o1 + reach.min(past_hi * 0.5)))
        || (past_lo > SAME_SURFACE_MM && probe_at(o0 - reach.min(past_lo * 0.5)))
}

/// Correct a mesh clearance between two BRep solids with the parallel
/// cylinder closed form, when (and only when) that pair provably produced
/// the mesh number — see the module docs for the rules. `segments_*` are the
/// densities the solids tessellate at (what the mesh was measured on).
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
    // Cheap prefilter before tessellating anything: some parallel pair whose
    // closed form is within reach of the mesh number.
    let plausible = ca.iter().any(|x| {
        cb.iter().any(|y| {
            line_angle(x.axis, y.axis) <= PARALLEL_RAD
                && closed_form(x, y).is_some_and(|g| {
                    (g - mesh.distance).abs() <= 0.25 * x.radius.min(y.radius).max(1.0)
                })
        })
    });
    if !plausible {
        return unrefined;
    }
    let (ta, tb) = (
        tessellate_brep(a, segments_a),
        tessellate_brep(b, segments_b),
    );

    // Rules 1 + 2 with measured bands; the smallest explaining gap wins.
    let mut best: Option<(f64, CylSurface, CylSurface)> = None;
    for x in &ca {
        for y in &cb {
            if line_angle(x.axis, y.axis) > PARALLEL_RAD {
                continue;
            }
            let Some(gap) = closed_form(x, y) else {
                continue;
            };
            if best.as_ref().is_some_and(|(g, _, _)| *g <= gap) {
                continue;
            }
            let (sx, sy) = (surface_of(&ca, x, &ta), surface_of(&cb, y, &tb));
            if !on_surface(&sx, mesh.point_a, sx.band) || !on_surface(&sy, mesh.point_b, sy.band) {
                continue;
            }
            let (y0, y1) = span_in(&sy, &sx.c);
            if sx.span.1.min(y1) - sx.span.0.max(y0) <= SAME_SURFACE_MM {
                continue; // no real common span
            }
            if (gap - mesh.distance).abs() > sx.band + sy.band {
                continue;
            }
            best = Some((gap, sx, sy));
        }
    }
    let Some((gap, sx, sy)) = best else {
        return unrefined;
    };
    let band = sx.band + sy.band;

    // Rule 3a: the rest of the geometry is not nearer (or crossing).
    if let Some(r) = mesh_surface_distance(
        &mesh_without(&ta, &sx.ordinals),
        &mesh_without(&tb, &sy.ordinals),
    ) {
        // A remaining witness on both cylinders' rims is the same feature
        // seen through its neighbours' chords, not a rival.
        let on_pair = on_surface(&sx, r.point_a, band) && on_surface(&sy, r.point_b, band);
        if !on_pair && (r.intersecting || r.distance < gap) {
            return unrefined;
        }
    }
    // Rule 3b: neither cylinder runs past the other's end into material.
    if runs_into(&sx, &sy, &MeshInsideTester::new(&tb))
        || runs_into(&sy, &sx, &MeshInsideTester::new(&ta))
    {
        return unrefined;
    }

    RefinedClearance {
        result: ClearanceResult {
            distance: gap,
            intersecting: gap < -CONTACT_EPS_MM,
            point_a: project(&sx.c, mesh.point_a),
            point_b: project(&sy.c, mesh.point_b),
        },
        basis: ClearanceBasis::AnalyticCylinders,
        mesh_distance: mesh.distance,
        chord_band_mm: Some(band),
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
    use super::ClearanceBasis;
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

    /// A mesh witness on the shaft/bore pair mid-span (what the real mesh
    /// reports for the 7.98-in-8.00 fit: shaft vertex at r 7.98, bore chord
    /// at ≈ r 7.999), for driving rule 3 directly.
    fn pair_witness() -> vcad_kernel_tessellate::ClearanceResult {
        vcad_kernel_tessellate::ClearanceResult {
            distance: 0.019,
            intersecting: false,
            point_a: [-7.98, 0.0, 10.0],
            point_b: [-7.999, 0.0, 10.0],
        }
    }

    #[test]
    fn coaxial_shaft_in_bore_reports_the_true_radial_gap() {
        let (shaft, block) = shaft_in_bore(7.98, 8.0);
        let r = shaft.clearance_refined(&block).expect("measurable");
        assert_eq!(r.basis, ClearanceBasis::AnalyticCylinders, "{r:?}");
        assert!(!r.result.intersecting, "{r:?}");
        assert!((r.result.distance - 0.02).abs() < 1e-9, "{r:?}");
        // The mesh number was chord-shortened, and the band is tight (the
        // boolean bore is sag-adaptive; the 32-gon shaft dominates).
        assert!(r.mesh_distance < 0.02, "{r:?}");
        assert!(r.chord_band_mm.unwrap() < 0.1, "{r:?}");
        // The plain API agrees, in either argument order.
        let c = shaft.clearance(&block).unwrap();
        assert!(!c.intersecting && (c.distance - 0.02).abs() < 1e-9, "{c:?}");
        let c = block.clearance(&shaft).unwrap();
        assert!(!c.intersecting && (c.distance - 0.02).abs() < 1e-9, "{c:?}");
    }

    #[test]
    fn d_shaft_in_round_bore_reads_the_radial_gap() {
        // The snowthrower adapter's motor shaft: r 7.98 with a flat 6 mm
        // off-axis (flat-to-round 14), itself a boolean, in an r 8 bore.
        let (round, block) = shaft_in_bore(7.98, 8.0);
        let flat_cut = Solid::cube(3.0, 20.0, 40.0).translate(-9.0, -10.0, -10.0);
        let shaft = round.difference(&flat_cut);
        let c = shaft.clearance(&block).unwrap();
        assert!(!c.intersecting, "{c:?}");
        assert!((c.distance - 0.02).abs() < 1e-9, "{c:?}");
    }

    #[test]
    fn oversized_shaft_still_interferes_with_exact_depth() {
        let (shaft, block) = shaft_in_bore(8.02, 8.0);
        let c = shaft.clearance(&block).unwrap();
        assert!(c.intersecting, "{c:?}");
        assert!((c.distance + 0.02).abs() < 1e-9, "{c:?}");
    }

    #[test]
    fn line_to_line_fit_is_contact_not_interference() {
        let (shaft, block) = shaft_in_bore(8.0, 8.0);
        let r = shaft.clearance_refined(&block).unwrap();
        if r.basis == ClearanceBasis::AnalyticCylinders {
            assert!(!r.result.intersecting, "{r:?}");
            assert!(r.result.distance.abs() < 1e-9, "{r:?}");
        }
    }

    #[test]
    fn pair_witness_is_replaced_by_the_closed_form() {
        let (shaft, block) = shaft_in_bore(7.98, 8.0);
        let r = shaft.refine_clearance(&block, &pair_witness());
        assert_eq!(r.basis, ClearanceBasis::AnalyticCylinders, "{r:?}");
        assert!((r.result.distance - 0.02).abs() < 1e-9, "{r:?}");
    }

    #[test]
    fn a_collar_sunk_into_the_face_is_not_hidden() {
        // Rule 3a/3b: a 10 mm-radius collar sunk 0.005 mm into the block's
        // top face — a real interference shallower than the shaft's chords.
        let (shaft, block) = shaft_in_bore(7.98, 8.0);
        let collar = Solid::cylinder(10.0, 3.0, 32).translate(0.0, 0.0, 19.995);
        let shaft = shaft.union(&collar);
        let r = shaft.refine_clearance(&block, &pair_witness());
        assert_eq!(r.basis, ClearanceBasis::Mesh, "{r:?}");
        let c = shaft.clearance(&block).unwrap();
        assert!(c.intersecting && c.distance < 0.0, "{c:?}");
    }

    #[test]
    fn a_shaft_driven_through_a_blind_bore_floor_is_not_hidden() {
        // Rule 3b: the bore stops at z 5 (blind from the top) and the shaft
        // reaches z 4.95 — the shaft's own cylinder face crosses the floor,
        // which leaving the cylinder faces out (3a) cannot see.
        let block = Solid::cube(20.0, 20.0, 20.0)
            .translate(-10.0, -10.0, 0.0)
            .difference(&Solid::cylinder(8.0, 20.0, 32).translate(0.0, 0.0, 5.0));
        let shaft = Solid::cylinder(7.98, 20.0, 32).translate(0.0, 0.0, 4.95);
        let r = shaft.refine_clearance(&block, &pair_witness());
        assert_eq!(r.basis, ClearanceBasis::Mesh, "{r:?}");
        let c = shaft.clearance(&block).unwrap();
        assert!(c.intersecting, "{c:?}");
        // Control: stopped 0.05 short of the floor, it is the plain fit.
        let shaft = Solid::cylinder(7.98, 20.0, 32).translate(0.0, 0.0, 5.05);
        let r = shaft.refine_clearance(&block, &pair_witness());
        assert_eq!(r.basis, ClearanceBasis::AnalyticCylinders, "{r:?}");
    }

    #[test]
    fn a_shoulder_inside_the_bore_is_not_hidden() {
        // Rule 3b, the other way round: the shaft steps up to r 9 at z 15,
        // inside the r 8 bore — the bore's surface past the shaft face's end
        // is inside the shaft.
        let (_, block) = shaft_in_bore(7.98, 8.0);
        let shaft = Solid::cylinder(7.98, 20.0, 32)
            .translate(0.0, 0.0, -5.0)
            .union(&Solid::cylinder(9.0, 10.0, 32).translate(0.0, 0.0, 15.0));
        let r = shaft.refine_clearance(&block, &pair_witness());
        assert_eq!(r.basis, ClearanceBasis::Mesh, "{r:?}");
    }

    #[test]
    fn a_witness_off_the_cylinders_is_left_alone() {
        let (shaft, block) = shaft_in_bore(7.98, 8.0);
        let mut m = pair_witness();
        m.point_a = [-7.5, 0.0, 10.0];
        let r = shaft.refine_clearance(&block, &m);
        assert_eq!(r.basis, ClearanceBasis::Mesh, "{r:?}");
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
