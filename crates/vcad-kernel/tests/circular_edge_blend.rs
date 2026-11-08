//! Fillets and chamfers on circular edges: cylinder rims and the rims of a
//! cup made by a boolean.
//!
//! Reported 2026-11-07: `Solid::cylinder(15, 30).fillet(3)` failed with
//! "blend geometry diverged … the radius exceeds the available edge
//! length" — on a rim 94 mm around. The bounding-box sanity check built
//! the input's extent from its topology vertices, and a cylinder has one
//! seam vertex per rim, so the "box" was a line segment; underneath, the
//! per-edge builder could not trim a closed (single-vertex) rim at all.
//! The same failure blocked rounding the rim of a cup (a rose knee cover).
//!
//! Every expected volume below is derived exactly (Pappus) and compared
//! with the tessellated volume at a density where a polygonal circle
//! under-reads its area by < 0.01 %.

use std::f64::consts::PI;

use vcad_kernel::vcad_kernel_fillet::{BlendKey, BlendSection, EdgeQuery};
use vcad_kernel::vcad_kernel_geom::SurfaceKind;
use vcad_kernel::vcad_kernel_math::Point3;
use vcad_kernel::{BlendError, Solid};

/// Tessellation density for volume checks.
const SEGMENTS: u32 = 256;
/// Relative volume tolerance at that density.
const REL_TOL: f64 = 5e-4;

fn volume(s: &Solid) -> f64 {
    let mesh = s.to_mesh(SEGMENTS);
    assert!(
        mesh.boundary_edges().is_empty(),
        "blended solid must tessellate watertight"
    );
    // Signed tetrahedron sum (divergence theorem).
    let v = |i: u32| {
        let i = 3 * i as usize;
        [
            mesh.vertices[i] as f64,
            mesh.vertices[i + 1] as f64,
            mesh.vertices[i + 2] as f64,
        ]
    };
    mesh.indices
        .chunks_exact(3)
        .map(|t| {
            let (a, b, c) = (v(t[0]), v(t[1]), v(t[2]));
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum()
}

fn assert_volume(label: &str, s: &Solid, expected: f64) {
    let got = volume(s);
    let rel = (got - expected).abs() / expected;
    eprintln!("{label}: volume {got:.4} mm³, exact {expected:.4} mm³, rel err {rel:.2e}");
    assert!(rel < REL_TOL, "{label}: {got} vs exact {expected}");
}

/// Volume swept by the fillet spandrel (corner square minus quarter disk,
/// side `rho`) at a rim of radius `rim_r`. `outward` = the square lies
/// outside the rim radius (an inner rim / bore); otherwise inside it (an
/// outer rim). 2π · Σ(area · centroid radius).
fn spandrel_volume(rim_r: f64, rho: f64, outward: bool) -> f64 {
    let s = if outward { 1.0 } else { -1.0 };
    let square = rho * rho * (rim_r + s * rho / 2.0);
    let quarter = PI * rho * rho / 4.0 * (rim_r + s * (rho - 4.0 * rho / (3.0 * PI)));
    2.0 * PI * (square - quarter)
}

/// Volume swept by a 45° chamfer triangle (legs `d`) at a rim of radius
/// `rim_r`; the triangle's centroid sits d/3 from the rim.
fn chamfer_volume(rim_r: f64, d: f64, outward: bool) -> f64 {
    let s = if outward { 1.0 } else { -1.0 };
    2.0 * PI * (d * d / 2.0) * (rim_r + s * d / 3.0)
}

/// Count the faces of `s` on each kind of surface.
fn faces_of(s: &Solid, kind: SurfaceKind) -> usize {
    let b = s.brep().expect("B-rep result");
    b.topology
        .faces
        .iter()
        .filter(|(_, f)| b.geometry.surfaces[f.surface_index].surface_type() == kind)
        .count()
}

fn cylinder() -> Solid {
    Solid::cylinder(15.0, 30.0, 64)
}

const CUP_R_OUT: f64 = 86.0;
const CUP_R_IN: f64 = 84.0;
const CUP_H: f64 = 40.0;
const CUP_FLOOR: f64 = 2.0;

/// The rose knee cover: a 2 mm-walled cup with a 2 mm floor.
fn cup() -> Solid {
    Solid::cylinder(CUP_R_OUT, CUP_H, 64)
        .difference(&Solid::cylinder(CUP_R_IN, CUP_H, 64).translate(0.0, 0.0, CUP_FLOOR))
}

fn cup_volume() -> f64 {
    PI * (CUP_R_OUT * CUP_R_OUT * CUP_H - CUP_R_IN * CUP_R_IN * (CUP_H - CUP_FLOOR))
}

fn key(shape: f64, size: f64) -> [BlendKey; 1] {
    [BlendKey {
        t: 0.0,
        section: BlendSection { shape, size },
    }]
}

#[test]
fn cylinder_fillet_both_rims_matches_pappus() {
    let r = cylinder().fillet(3.0).expect("r=3 fits a r15 h30 cylinder");
    let expected = PI * 15.0 * 15.0 * 30.0 - 2.0 * spandrel_volume(15.0, 3.0, false);
    assert_volume("cylinder fillet 3", &r, expected);
    // Exact blend surfaces, not facets.
    assert_eq!(faces_of(&r, SurfaceKind::Torus), 2);
}

#[test]
fn cylinder_chamfer_both_rims_matches_pappus() {
    let r = cylinder()
        .chamfer(2.0)
        .expect("d=2 fits a r15 h30 cylinder");
    let expected = PI * 15.0 * 15.0 * 30.0 - 2.0 * chamfer_volume(15.0, 2.0, false);
    assert_volume("cylinder chamfer 2", &r, expected);
    assert_eq!(faces_of(&r, SurfaceKind::Cone), 2);
}

#[test]
fn cylinder_picked_top_rim_fillet() {
    let r = cylinder()
        .edge_blend(
            &EdgeQuery::Near {
                point: Point3::new(0.0, 15.0, 30.0),
            },
            &key(1.0, 3.0),
        )
        .expect("picked top rim");
    let expected = PI * 15.0 * 15.0 * 30.0 - spandrel_volume(15.0, 3.0, false);
    assert_volume("cylinder top-rim fillet 3", &r, expected);
}

#[test]
fn cylinder_whole_solid_edge_blend_chamfer() {
    let r = cylinder()
        .edge_blend(&EdgeQuery::All, &key(0.0, 2.0))
        .expect("edge_blend All chamfer");
    let expected = PI * 15.0 * 15.0 * 30.0 - 2.0 * chamfer_volume(15.0, 2.0, false);
    assert_volume("cylinder edge_blend chamfer 2", &r, expected);
}

#[test]
fn cup_outer_top_rim_fillet() {
    let r = cup()
        .edge_blend(
            &EdgeQuery::Near {
                point: Point3::new(CUP_R_OUT, 0.0, CUP_H),
            },
            &key(1.0, 1.0),
        )
        .expect("fillet 1 on the cup's outer top rim");
    let expected = cup_volume() - spandrel_volume(CUP_R_OUT, 1.0, false);
    assert_volume("cup outer-rim fillet 1", &r, expected);
}

#[test]
fn cup_inner_top_rim_chamfer() {
    let r = cup()
        .edge_blend(
            &EdgeQuery::Near {
                point: Point3::new(0.0, -CUP_R_IN, CUP_H),
            },
            &key(0.0, 0.5),
        )
        .expect("chamfer 0.5 on the cup's inner top rim");
    let expected = cup_volume() - chamfer_volume(CUP_R_IN, 0.5, true);
    assert_volume("cup inner-rim chamfer 0.5", &r, expected);
}

/// Whole-solid fillet 1 on the cup: outer bottom, outer top and inner top
/// rims lose a spandrel; the concave floor rim gains one. On the 2 mm top
/// face the two r=1 rims meet exactly — a full round.
#[test]
fn cup_whole_solid_fillet_includes_concave_floor_rim() {
    let r = cup().fillet(1.0).expect("fillet 1 fits the cup");
    let expected = cup_volume()
        - 2.0 * spandrel_volume(CUP_R_OUT, 1.0, false)
        - spandrel_volume(CUP_R_IN, 1.0, true)
        + spandrel_volume(CUP_R_IN, 1.0, false);
    assert_volume("cup whole-solid fillet 1", &r, expected);
}

#[test]
fn over_limit_radius_is_refused() {
    // Cap radius: a fillet of 15 on a r15 cylinder.
    let e = cylinder().fillet(15.0).expect_err("r=15 cannot fit r15");
    assert!(matches!(e, BlendError::DoesNotFit(_)), "{e}");
    eprintln!("cylinder fillet 15: {e}");
    // Wall thickness: a 2 mm fillet on a 2 mm cup rim.
    let e = cup()
        .edge_blend(
            &EdgeQuery::Near {
                point: Point3::new(CUP_R_OUT, 0.0, CUP_H),
            },
            &key(1.0, 2.0),
        )
        .expect_err("r=2 cannot fit a 2 mm wall");
    assert!(e.to_string().contains("wall thickness"), "{e}");
    eprintln!("cup rim fillet 2: {e}");
    // Whole-solid chamfer 1.5 on the cup: 1.5 + 1.5 > 2 mm wall.
    let e = cup().chamfer(1.5).expect_err("2×1.5 > 2 mm wall");
    assert!(matches!(e, BlendError::DoesNotFit(_)), "{e}");
}
