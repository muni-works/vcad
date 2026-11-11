//! Repros from modelling K1 printed hand parts (coaxial counterbores,
//! slivers, coplanar tool faces, many-tool differences). Each fixture must
//! evaluate without a panic to a closed mesh whose signed volume does not
//! change under translation.

use std::collections::HashMap;

use vcad_eval::{evaluate_document, EvalOptions};
use vcad_loon::eval_vcad;

fn open_edges(positions: &[f32], indices: &[u32]) -> usize {
    let key = |i: u32| {
        let v = &positions[i as usize * 3..i as usize * 3 + 3];
        [v[0].to_bits(), v[1].to_bits(), v[2].to_bits()]
    };
    let mut net = HashMap::<_, i32>::new();
    for t in indices.chunks_exact(3) {
        for k in 0..3 {
            let (a, b) = (key(t[k]), key(t[(k + 1) % 3]));
            if a == b {
                continue;
            }
            let (lo, hi, d) = if a < b { (a, b, 1) } else { (b, a, -1) };
            *net.entry((lo, hi)).or_default() += d;
        }
    }
    net.values().map(|n| n.unsigned_abs() as usize).sum()
}

fn signed_volume(positions: &[f32], indices: &[u32]) -> f64 {
    let p = |i: u32| {
        let v = &positions[i as usize * 3..i as usize * 3 + 3];
        [v[0] as f64, v[1] as f64, v[2] as f64]
    };
    indices
        .chunks_exact(3)
        .map(|t| {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum()
}

/// `(open edges, mesh signed volume)` of the single root part.
fn eval(src: &str) -> (usize, f64) {
    let doc = eval_vcad(src, None).expect("loon eval");
    let scene = evaluate_document(&doc, &EvalOptions::default()).expect("evaluates");
    assert_eq!(scene.parts.len(), 1, "one root part");
    let m = &scene.parts[0].mesh;
    assert!(!m.indices.is_empty(), "non-empty mesh");
    (
        open_edges(&m.positions, &m.indices),
        signed_volume(&m.positions, &m.indices),
    )
}

/// Wrap every `[root X "mat"]` body in a translation.
fn translated(src: &str) -> String {
    let i = src.rfind("[root ").expect("root form");
    let body = &src[i + 6..];
    let j = body.rfind(" \"").expect("material");
    format!(
        "{}[root [translate 100 -70 55 {}]{}",
        &src[..i],
        &body[..j],
        &body[j..]
    )
}

fn assert_closed(name: &str, src: &str) {
    let (open, vol) = eval(src);
    let (open_t, vol_t) = eval(&translated(src));
    assert!(vol > 0.0, "{name}: signed volume {vol}");
    assert_eq!(
        (open, open_t),
        (0, 0),
        "{name}: open edges (vol {vol} / {vol_t})"
    );
    assert!(
        (vol - vol_t).abs() <= 1e-3 * vol,
        "{name}: volume {vol} at origin vs {vol_t} translated"
    );
}

macro_rules! fixture {
    ($test:ident, $file:literal) => {
        #[test]
        fn $test() {
            assert_closed($file, include_str!(concat!("fixtures/k1/", $file)));
        }
    };
}

// 1: coaxial counterbore + through hole on a half-cylinder (was a wasm trap).
// Needs both the union seam (1u) and the counterbore entering the curved
// wall: a perpendicular unequal-radius cylinder pair (1r).
#[test]
fn coaxial_counterbore_half_cylinder() {
    assert_closed("i1", include_str!("fixtures/k1/i1_counterbore.loon"));
}
// 1u: the union under 1 on its own. The flange top is `-8.3 + 8` =
// -0.3000000000000007 and the half-cylinder base is -0.3; that float noise
// left 132 open edges until booleans snapped it onto one plane.
fixture!(flange_half_cylinder_union, "i1u_flange_half_union.loon");
// 1r: a blind radial hole into a cylinder wall. Perpendicular cylinders of
// unequal radius used to fall to an unordered marching sampler; the hole
// lost its floor.
fixture!(blind_radial_hole_in_cylinder, "i1r_radial_hole.loon");
// 1b: the same hole as a revolve of a profile touching its axis.
#[test]
fn revolve_profile_touching_axis() {
    assert_closed("i1b", include_str!("fixtures/k1/i1b_revolve.loon"));
}
// 2: mixed facet counts on a coaxial hole + counterbore.
fixture!(mixed_facet_counterbore, "i2_mixed1.loon");
fixture!(mixed_facet_counterbore_8x, "i2_mixed8.loon");
// 3: 0.2 mm sliver between counterbore floor and bore ceiling.
fixture!(counterbore_sliver, "i3a_sliver.loon");
// 3b: hex prism standing exactly on a disc's top face, unioned.
fixture!(hex_prism_on_disc_face, "i3b_hexdisc.loon");
// 4: half-annulus slot grazing a unioned hub. The slot itself (box ∩
// annulus) was the defect: the box side cuts each annular cap through its
// hole, and that chord used to be refused (4a).
fixture!(box_cuts_annulus_through_hole, "i4a_box_annulus.loon");
#[test]
fn slot_across_hub_seam() {
    assert_closed("i4", include_str!("fixtures/k1/i4_slot_hub.loon"));
}
// 5: two difference tools with coplanar flat ends on x = 0.
fixture!(coplanar_slot_ends, "i5_coplanar_ends.loon");
fixture!(grip_cam, "i5b_gripcam.loon");
// 6: ring minus 4x (rotated pocket + hole) minus 4 magnet seats, in one eval.
fixture!(stator_ring_many_tools, "i6_stator.loon");
// 8: wrist adapter (render/STEP trap report).
fixture!(wrist_adapter, "i8_wrist.loon");

/// 7: an unused `[let f [fn ...]]` binding must not break evaluation.
#[test]
fn unused_fn_binding() {
    assert_closed("i7", include_str!("fixtures/k1/i7_unused_fn.loon"));
}

/// 8: STEP export of the wrist adapter must not panic.
#[test]
fn wrist_adapter_step_export() {
    let doc = eval_vcad(include_str!("fixtures/k1/i8_wrist.loon"), None).expect("loon eval");
    let scene = evaluate_document(&doc, &EvalOptions::default()).expect("evaluates");
    let solid = scene.parts[0].solid.as_ref().expect("solid");
    if let Some(brep) = solid.as_brep() {
        let buf = vcad_kernel_step::write_step_to_buffer(brep).expect("STEP write");
        assert!(!buf.is_empty());
    }
}

/// A counterbore breaking out into a parallel, off-axis bore: the walls meet
/// in two shared generators. The SSI used to return nothing for parallel
/// cylinders, so at x = 100 the cut was silently dropped (Analytic result,
/// volume of the uncut ring) and at x = 0 only the mesh fallback saved it.
#[test]
fn counterbore_breaking_into_bore_is_analytic_anywhere() {
    use vcad_kernel::{Solid, SolidFidelity};
    for x in [0.0, 100.0] {
        let ring = Solid::cylinder(25.0, 8.0, 96)
            .difference(&Solid::cylinder(18.5, 10.0, 96).translate(0.0, 0.0, -1.0))
            .translate(x, 0.0, 0.0);
        let cb = Solid::cylinder(2.9, 6.0, 40).translate(20.75 + x, 0.0, 3.0);
        let cut = ring.difference(&cb);
        assert_eq!(cut.fidelity(), SolidFidelity::Analytic, "x = {x}");
        assert!(
            (cut.volume() - 6981.61).abs() < 0.5,
            "x = {x}: volume {} (uncut ring is 7106)",
            cut.volume()
        );
        let m = cut.to_mesh(32);
        assert_eq!(open_edges(&m.vertices, &m.indices), 0, "x = {x}");
    }
}
