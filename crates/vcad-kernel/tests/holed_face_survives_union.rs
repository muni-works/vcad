//! A face with a round hole that the other operand never touches must come
//! through a union intact.
//!
//! Found on the launch page's live bracket: a drilled base plate unioned with
//! an upright lost its whole bottom face (the drilled one), so the part
//! rendered hollow from below. The volume was still right, because the bottom
//! face sits at z = 0 and contributes nothing to the divergence-theorem
//! volume, which is exactly why no volume test caught it. A square hole in
//! the same place survives; a round one did not.

use vcad_kernel::Solid;

/// Total area of exported triangles lying in the plane z = `z`.
fn area_in_plane_z(verts: &[f32], idx: &[u32], z: f32) -> f64 {
    let mut area = 0.0;
    for t in idx.chunks(3) {
        let p = |i: u32| {
            let k = i as usize * 3;
            [verts[k] as f64, verts[k + 1] as f64, verts[k + 2] as f64]
        };
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        if [a[2], b[2], c[2]]
            .iter()
            .all(|v| (*v - z as f64).abs() < 1e-6)
        {
            area += ((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])).abs() / 2.0;
        }
    }
    area
}

fn bottom_area(s: &Solid) -> f64 {
    let m = s.to_mesh(48);
    area_in_plane_z(&m.vertices, &m.indices, 0.0)
}

#[test]
fn round_holed_bottom_face_survives_union_with_disjoint_upright() {
    let (w, d, t) = (90.0, 50.0, 6.0);
    let plate = Solid::cube(w, d, t)
        .difference(&Solid::cylinder(3.0, 18.0, 48).translate(22.0, 21.0, -6.0));
    let expected = w * d - std::f64::consts::PI * 9.0;
    let before = bottom_area(&plate);
    assert!(
        (before - expected).abs() < 2.0,
        "plate alone: bottom area {before:.1}, expected ≈{expected:.1}"
    );

    // The upright starts 0.5 mm above the bottom: it never touches that face.
    let upright = Solid::cube(w, t, 50.0).translate(0.0, d - t, 0.5);
    let fused = plate.union(&upright);
    let after = bottom_area(&fused);
    assert!(
        (after - expected).abs() < 2.0,
        "bottom face lost in union: area {after:.1}, expected ≈{expected:.1}"
    );
}

#[test]
fn launch_bracket_keeps_its_drilled_bottom() {
    // The exact part on the launch page (hero-worker.js `build`).
    let (w, d, t, h, n, hole, bore) = (90.0, 50.0, 6.0, 50.0, 3usize, 6.0, 18.0);
    let pitch = w / (n as f64 + 1.0);
    let mut plate = Solid::cube(w, d, t);
    for i in 0..n {
        plate = plate.difference(&Solid::cylinder(hole / 2.0, t * 3.0, 48).translate(
            pitch * (i as f64 + 1.0),
            d * 0.42,
            -t,
        ));
    }
    let wall = Solid::cube(w, t, h).difference(
        &Solid::cylinder(bore / 2.0, t * 3.0, 48)
            .rotate(90.0, 0.0, 0.0)
            .translate(w / 2.0, t * 2.0, h * 0.58),
    );
    let part = plate.union(&wall.translate(0.0, d - t, 0.0));
    let expected = w * d - n as f64 * std::f64::consts::PI * (hole / 2.0) * (hole / 2.0);
    let got = bottom_area(&part);
    assert!(
        (got - expected).abs() < 3.0,
        "bracket bottom area {got:.1}, expected ≈{expected:.1}"
    );
}
