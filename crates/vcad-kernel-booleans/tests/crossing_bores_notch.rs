//! Regression: two equal-radius bores crossing inside a block, then a notch
//! that clips the bottom of one bore (vcad.app field report, 2026-11-15).
//!
//! ```text
//! [pipe [cube 40 40 60]
//!   [difference X bore r4 through (y 20, z 35)]
//!   [difference Y bore r4 through (x 20, z 35)]
//!   [difference [translate 25 -1 20 [cube 20 42 12]]]]
//! ```
//!
//! Equal-radius perpendicular cylinders meet in the Steinmetz pair of
//! ellipses, which cross at two saddle points. The pipeline routed that
//! figure-8 to a cylindrical splitter that silently returned both faces
//! unsplit, so the second bore removed ~670 mm³ instead of ~1669 mm³; the
//! result was then swapped for (or kept as) triangle soup, and the notch cut
//! on that soup came back with ~600 non-manifold and ~500 folded edges —
//! the dark triangle lines the Mac app's feature-edge overlay drew.

use std::collections::HashMap;
use std::f64::consts::PI;

use vcad_kernel_booleans::{boolean_op_reported, BooleanOp, BooleanResult, Fidelity};
use vcad_kernel_math::Transform;
use vcad_kernel_primitives::{make_cube, make_cylinder, BRepSolid};
use vcad_kernel_tessellate::TriangleMesh;

const SEGMENTS: u32 = 32;
const R: f64 = 4.0;

fn transformed(mut brep: BRepSolid, t: &Transform) -> BRepSolid {
    for (_, v) in &mut brep.topology.vertices {
        v.point = t.apply_point(&v.point);
    }
    brep.geometry.surfaces = brep
        .geometry
        .surfaces
        .drain(..)
        .map(|s| s.transform(t))
        .collect();
    brep
}

/// r4 × 42 bore along +X from x = −1, axis through (y, z).
fn x_bore(y: f64, z: f64) -> BRepSolid {
    // `a.then(b)` is a·b: `b` acts first.
    let t = Transform::translation(-1.0, y, z).then(&Transform::rotation_y(PI / 2.0));
    transformed(make_cylinder(R, 42.0, SEGMENTS), &t)
}

/// r4 × 42 bore along +Y from y = −1, axis through (x, z).
fn y_bore(x: f64, z: f64) -> BRepSolid {
    let t = Transform::translation(x, -1.0, z).then(&Transform::rotation_x(-PI / 2.0));
    transformed(make_cylinder(R, 42.0, SEGMENTS), &t)
}

fn brep(r: &BooleanResult) -> BRepSolid {
    r.as_brep().expect("brep result").clone()
}

fn signed_volume(mesh: &TriangleMesh) -> f64 {
    let p = |i: u32| {
        let i = i as usize * 3;
        [
            mesh.vertices[i] as f64,
            mesh.vertices[i + 1] as f64,
            mesh.vertices[i + 2] as f64,
        ]
    };
    mesh.indices
        .chunks(3)
        .map(|t| {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum()
}

/// (non-manifold edges, folded edges, boundary edges) of a mesh welded at
/// 1e-5 — the same census the Mac app's feature-edge overlay is fed.
/// "Folded" = two triangles sharing an edge whose normals are within ~25°
/// of opposite (a sheet doubled back on itself).
fn edge_census(mesh: &TriangleMesh) -> (usize, usize, usize) {
    let key = |vi: u32| -> [i64; 3] {
        let i = vi as usize * 3;
        [
            (mesh.vertices[i] as f64 * 1e5).round() as i64,
            (mesh.vertices[i + 1] as f64 * 1e5).round() as i64,
            (mesh.vertices[i + 2] as f64 * 1e5).round() as i64,
        ]
    };
    let normal = |t: &[u32]| {
        let p = |i: u32| {
            let i = i as usize * 3;
            [
                mesh.vertices[i] as f64,
                mesh.vertices[i + 1] as f64,
                mesh.vertices[i + 2] as f64,
            ]
        };
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if l > 0.0 {
            [n[0] / l, n[1] / l, n[2] / l]
        } else {
            [0.0; 3]
        }
    };
    let mut adj: HashMap<([i64; 3], [i64; 3]), Vec<usize>> = HashMap::new();
    for (ti, t) in mesh.indices.chunks(3).enumerate() {
        let k = [key(t[0]), key(t[1]), key(t[2])];
        for i in 0..3 {
            let (a, b) = (k[i], k[(i + 1) % 3]);
            if a == b {
                continue;
            }
            adj.entry(if a < b { (a, b) } else { (b, a) })
                .or_default()
                .push(ti);
        }
    }
    let (mut nonmanifold, mut folded, mut boundary) = (0, 0, 0);
    for ts in adj.values() {
        match ts.len() {
            1 => boundary += 1,
            2 => {
                let n0 = normal(&mesh.indices[ts[0] * 3..ts[0] * 3 + 3]);
                let n1 = normal(&mesh.indices[ts[1] * 3..ts[1] * 3 + 3]);
                if n0[0] * n1[0] + n0[1] * n1[1] + n0[2] * n1[2] < -0.9 {
                    folded += 1;
                }
            }
            _ => nonmanifold += 1,
        }
    }
    (nonmanifold, folded, boundary)
}

/// Area of the circular segment of radius `r` below a chord at depth `h`.
fn segment_area(r: f64, h: f64) -> f64 {
    r * r * ((r - h) / r).acos() - (r - h) * (2.0 * r * h - h * h).sqrt()
}

fn check(label: &str, result: &BooleanResult, expected: f64, tol_frac: f64) {
    let mesh = result.to_mesh(SEGMENTS);
    let vol = signed_volume(&mesh);
    let (nonmanifold, folded, boundary) = edge_census(&mesh);
    eprintln!(
        "{label}: volume {vol:.2} (truth {expected:.2}) tris {} nonmanifold {nonmanifold} \
         folded {folded} boundary {boundary}",
        mesh.indices.len() / 3
    );
    assert_eq!(nonmanifold, 0, "{label}: non-manifold edges");
    assert_eq!(folded, 0, "{label}: folded edges");
    assert_eq!(boundary, 0, "{label}: boundary edges");
    let rel = (vol - expected).abs() / expected;
    assert!(
        rel <= tol_frac,
        "{label}: volume {vol:.2} off truth {expected:.2} by {:.3}%",
        rel * 100.0
    );
}

#[test]
fn equal_radius_crossing_bores_then_notch() {
    let block = make_cube(40.0, 40.0, 60.0);
    let bore = PI * R * R * 40.0;
    // Steinmetz bicylinder: the shared volume of the two bores.
    let steinmetz = 16.0 * R * R * R / 3.0;

    let (r1, _) = boolean_op_reported(&block, &x_bore(20.0, 35.0), BooleanOp::Difference, SEGMENTS)
        .expect("x bore");
    let v1 = 96_000.0 - bore;
    check("x bore", &r1, v1, 5e-4);

    let (r2, rep2) = boolean_op_reported(
        &brep(&r1),
        &y_bore(20.0, 35.0),
        BooleanOp::Difference,
        SEGMENTS,
    )
    .expect("y bore");
    let v2 = v1 - (bore - steinmetz);
    check("crossing y bore", &r2, v2, 5e-4);
    assert_eq!(
        rep2.fidelity,
        Fidelity::Analytic,
        "the crossing bore must be cut analytically, not swapped for soup: {rep2:?}"
    );

    // Notch x 25..45 (clipped to 40), z 20..32; its top face clips the X
    // bore (axis z 35, r 4) by 1 mm over x 25..40.
    let notch = transformed(
        make_cube(20.0, 42.0, 12.0),
        &Transform::translation(25.0, -1.0, 20.0),
    );
    let (r3, rep3) =
        boolean_op_reported(&brep(&r2), &notch, BooleanOp::Difference, SEGMENTS).expect("notch");
    let v3 = v2 - (15.0 * 40.0 * 12.0 - 15.0 * segment_area(R, 1.0));
    check("notch", &r3, v3, 5e-4);
    eprintln!("notch report: {rep3:?}");
}
