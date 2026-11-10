//! The viewport meshes curved faces to a chord tolerance (no visible
//! 32-gons on big radii). Every solid must stay watertight at that density
//! and land closer to its exact volume than the fixed-count mesh.

use std::f64::consts::PI;
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

fn check(name: &str, s: &Solid, exact: f64) {
    let coarse = s.to_mesh(32);
    let fine = s.to_mesh_with_chord(32, 0.02);
    assert!(
        fine.boundary_edges().is_empty(),
        "{name}: {} open edges",
        fine.boundary_edges().len()
    );
    let (ec, ef) = ((vol(&coarse) - exact).abs(), (vol(&fine) - exact).abs());
    assert!(
        ef <= ec + 1e-6 * exact,
        "{name}: fine err {ef} > coarse err {ec}"
    );
    assert!(
        fine.indices.len() >= coarse.indices.len(),
        "{name}: fine mesh is coarser"
    );
}

#[test]
fn big_cylinder() {
    check(
        "cyl r86",
        &Solid::cylinder(86.0, 40.0, 0),
        PI * 86.0 * 86.0 * 40.0,
    );
}

#[test]
fn sphere_and_cone() {
    check(
        "sphere r40",
        &Solid::sphere(40.0, 0),
        4.0 / 3.0 * PI * 40f64.powi(3),
    );
    check(
        "cone",
        &Solid::cone(30.0, 10.0, 25.0, 0),
        PI * 25.0 / 3.0 * (900.0 + 300.0 + 100.0),
    );
}

#[test]
fn cup() {
    let cup = Solid::cylinder(86.0, 40.0, 0)
        .difference(&Solid::cylinder(84.0, 40.0, 0).translate(0.0, 0.0, 2.0));
    let exact = PI * (86.0 * 86.0 * 40.0 - 84.0 * 84.0 * 38.0);
    check("cup", &cup, exact);
}

#[test]
fn plate_with_hole_and_filleted_box() {
    let plate = Solid::cube(60.0, 40.0, 5.0)
        .difference(&Solid::cylinder(8.0, 10.0, 0).translate(30.0, 20.0, -2.0));
    check("plate-hole", &plate, 60.0 * 40.0 * 5.0 - PI * 64.0 * 5.0);
    let fb = Solid::cube(30.0, 20.0, 10.0).fillet(3.0).expect("fillet");
    let m = fb.to_mesh_with_chord(32, 0.02);
    assert!(
        m.boundary_edges().is_empty(),
        "filleted box: {} open edges",
        m.boundary_edges().len()
    );
}
