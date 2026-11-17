//! The viewport meshes curved faces to a chord tolerance (no visible
//! 32-gons on big radii). Every solid must stay watertight at that density
//! and sit within the tolerance of its exact volume.
//!
//! The tolerance cuts both ways. A face the tessellator samples itself is
//! refined past the fixed count; a rim a boolean froze onto the kernel's
//! 1 µm grid is thinned down to the tolerance, so on a cut part the display
//! mesh is coarser than `to_mesh` — by design — and its volume error is
//! bounded by the tolerance times the curved area instead.

use std::f64::consts::PI;
use vcad_kernel::vcad_kernel_fillet::{BlendKey, BlendSection, EdgeQuery};
use vcad_kernel::vcad_kernel_math::Point3;
use vcad_kernel::vcad_kernel_tessellate::TriangleMesh;
use vcad_kernel::Solid;

fn vol(m: &TriangleMesh) -> f64 {
    let v = &m.vertices;
    m.indices
        .chunks_exact(3)
        .map(|t| {
            let p = |i: u32| {
                let i = 3 * i as usize;
                [v[i] as f64, v[i + 1] as f64, v[i + 2] as f64]
            };
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum()
}

const CHORD: f64 = 0.02;

/// `curved_area`: area of the solid's curved faces, which bounds what a
/// chord tolerance can cost in volume. `frozen_rims`: the solid carries
/// boolean rims, which the display mesh thins rather than refines.
fn check(name: &str, s: &Solid, exact: f64, curved_area: f64, frozen_rims: bool) {
    let coarse = s.to_mesh(32);
    let fine = s.to_mesh_with_chord(32, CHORD);
    assert!(
        fine.boundary_edges().is_empty(),
        "{name}: {} open edges",
        fine.boundary_edges().len()
    );
    let (ec, ef) = ((vol(&coarse) - exact).abs(), (vol(&fine) - exact).abs());
    if frozen_rims {
        assert!(
            ef <= CHORD * curved_area,
            "{name}: display err {ef} over the chord bound {}",
            CHORD * curved_area
        );
        assert!(
            fine.indices.len() <= coarse.indices.len(),
            "{name}: frozen rims were not thinned ({} vs {} indices)",
            fine.indices.len(),
            coarse.indices.len()
        );
    } else {
        assert!(
            ef <= ec + 1e-6 * exact,
            "{name}: fine err {ef} > coarse err {ec}"
        );
        assert!(
            fine.indices.len() >= coarse.indices.len(),
            "{name}: fine mesh is coarser"
        );
    }
}

#[test]
fn big_cylinder() {
    check(
        "cyl r86",
        &Solid::cylinder(86.0, 40.0, 0),
        PI * 86.0 * 86.0 * 40.0,
        2.0 * PI * 86.0 * 40.0,
        false,
    );
}

#[test]
fn sphere_and_cone() {
    check(
        "sphere r40",
        &Solid::sphere(40.0, 0),
        4.0 / 3.0 * PI * 40f64.powi(3),
        4.0 * PI * 1600.0,
        false,
    );
    check(
        "cone",
        &Solid::cone(30.0, 10.0, 25.0, 0),
        PI * 25.0 / 3.0 * (900.0 + 300.0 + 100.0),
        PI * 40.0 * (25f64 * 25.0 + 400.0).sqrt(),
        false,
    );
}

#[test]
fn cup_and_filleted_cup() {
    let cup = Solid::cylinder(86.0, 40.0, 0)
        .difference(&Solid::cylinder(84.0, 40.0, 0).translate(0.0, 0.0, 2.0));
    let exact = PI * (86.0 * 86.0 * 40.0 - 84.0 * 84.0 * 38.0);
    check(
        "cup",
        &cup,
        exact,
        2.0 * PI * (86.0 * 40.0 + 84.0 * 38.0),
        true,
    );
    let keys = [BlendKey {
        t: 0.0,
        section: BlendSection {
            size: 5.0,
            shape: 1.0,
        },
    }];
    let f = cup
        .edge_blend(
            &EdgeQuery::Near {
                point: Point3::new(86.0, 0.0, 0.0),
            },
            &keys,
        )
        .expect("rim fillet");
    let mesh = f.to_mesh_with_chord(32, 0.02);
    assert!(
        mesh.boundary_edges().is_empty(),
        "filleted cup: {} open edges",
        mesh.boundary_edges().len()
    );
}

#[test]
fn plate_with_hole_and_filleted_box() {
    let plate = Solid::cube(60.0, 40.0, 5.0)
        .difference(&Solid::cylinder(8.0, 10.0, 0).translate(30.0, 20.0, -2.0));
    check(
        "plate-hole",
        &plate,
        60.0 * 40.0 * 5.0 - PI * 64.0 * 5.0,
        2.0 * PI * 8.0 * 5.0,
        true,
    );
    let fb = Solid::cube(30.0, 20.0, 10.0).fillet(3.0).expect("fillet");
    let m = fb.to_mesh_with_chord(32, 0.02);
    assert!(
        m.boundary_edges().is_empty(),
        "filleted box: {} open edges",
        m.boundary_edges().len()
    );
}

/// The same solid thins to the same mesh every time: a warm viewport
/// session hands back a cached mesh and is compared against a cold one.
#[test]
fn display_mesh_is_deterministic() {
    let build = || {
        Solid::cube(10.0, 10.0, 10.0)
            .difference(&Solid::cylinder(3.0, 12.0, 32).translate(5.0, 0.0, 0.0))
            .to_mesh_with_chord(32, CHORD)
    };
    let first = build();
    for _ in 0..8 {
        let again = build();
        assert_eq!(first.indices, again.indices);
        assert_eq!(first.vertices, again.vertices);
        assert_eq!(first.face_ids, again.face_ids);
    }
}
