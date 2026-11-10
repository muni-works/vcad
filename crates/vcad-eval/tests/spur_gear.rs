//! End-to-end: the parametric `SpurGear` node evaluates to a real involute
//! solid, its volume agrees with closed-form estimates, the bore removes
//! exactly a cylinder, and a meshed pair placed by `vcad_ir::gear` clears at
//! the standard centre distance but interferes when pulled closer — measured
//! on the evaluated BRep solids, not the 2D profiles.

use std::f64::consts::PI;

use vcad_eval::{evaluate_document, EvalOptions};
use vcad_ir::gear::{center_distance, mesh_rotation_deg};
use vcad_kernel::Solid;
use vcad_loon::eval_vcad;

fn solids(src: &str) -> Vec<Solid> {
    let doc = eval_vcad(src, None).unwrap_or_else(|e| panic!("eval_vcad {src}: {e}"));
    let scene = evaluate_document(
        &doc,
        &EvalOptions {
            skip_clash_detection: true,
            clock: None,
            root_cache: None,
            mesh_segments: 0,
            on_root: None,
        },
    )
    .expect("evaluate_document");
    scene
        .parts
        .into_iter()
        .map(|p| p.solid.expect("root solid"))
        .collect()
}

fn one(src: &str) -> Solid {
    solids(src).remove(0)
}

/// Closed-form estimate: root disc plus half the tooth band.
fn estimate(m: f64, z: f64, fw: f64) -> f64 {
    let rt = m * z / 2.0 + m;
    let rr = m * z / 2.0 - 1.25 * m;
    (PI * rr * rr + PI * (rt * rt - rr * rr) / 2.0) * fw
}

#[test]
fn spur_gear_volume_matches_closed_form() {
    for (m, z, fw) in [(2.0, 20.0, 10.0), (1.0, 40.0, 5.0), (3.0, 12.0, 8.0)] {
        let v = one(&format!("[spur-gear {m} {z} 20 {fw} 0]")).volume();
        let est = estimate(m, z, fw);
        let rel = (v - est).abs() / est;
        assert!(
            rel < 0.04,
            "m{m} z{z}: volume {v:.2} vs estimate {est:.2} ({rel:.4})"
        );
        // Strictly between the root and tip cylinders.
        let rr = m * z / 2.0 - 1.25 * m;
        let rt = m * z / 2.0 + m;
        assert!(v > PI * rr * rr * fw && v < PI * rt * rt * fw);
    }
}

#[test]
fn bore_removes_a_cylinder() {
    let solid = one("[spur-gear 2 20 20 10 0]").volume();
    let bored = one("[spur-gear 2 20 20 10 8]").volume();
    let hole = PI * 4.0 * 4.0 * 10.0;
    // Arcs are polygonised; a 4 mm circle loses well under 1% of its area.
    let removed = solid - bored;
    assert!(
        (removed - hole).abs() / hole < 0.01,
        "bore removed {removed:.2} mm³, cylinder is {hole:.2}"
    );
}

#[test]
fn bad_gear_parameters_fail_loudly() {
    // A bore larger than the root circle can't make a gear.
    assert!(eval_vcad("[spur-gear 2 20 20 10 40]", None).is_err());
    // Too few teeth.
    assert!(eval_vcad("[spur-gear 2 3 20 10 0]", None).is_err());
}

fn pair(m: f64, z1: u32, z2: u32, shift: f64, extra_rot: f64) -> (Solid, Solid) {
    let a = center_distance(m, z1, z2) - shift;
    let r2 = mesh_rotation_deg(z1, 0.0, z2, 0.0) + extra_rot;
    let mut s = solids(&format!(
        "[spur-gear {m} {z1} 20 6 5]
         [translate {a} 0 0 [rotate 0 0 {r2} [spur-gear {m} {z2} 20 6 5]]]"
    ));
    let b = s.remove(1);
    (s.remove(0), b)
}

#[test]
fn meshed_pair_clears_at_center_distance_and_interferes_closer() {
    let (m, z1, z2) = (2.0, 20, 30);
    let (g1, g2) = pair(m, z1, z2, 0.0, 0.0);
    let c = g1.clearance(&g2).expect("clearance").distance;
    assert!(c >= -1e-6, "meshed pair interferes: {c:.6} mm");
    assert!(c < 0.05 * m, "meshed pair gap {c:.5} mm is not a mesh");

    let (g1, g2) = pair(m, z1, z2, 0.2 * m, 0.0);
    let c = g1.clearance(&g2).expect("clearance").distance;
    assert!(c < 0.0, "0.2 m closer should interfere, clearance {c:.5}");

    // Tooth on tooth: half a pitch out of phase.
    let (g1, g2) = pair(m, z1, z2, 0.0, 180.0 / z2 as f64);
    let c = g1.clearance(&g2).expect("clearance").distance;
    assert!(c < 0.0, "tooth-on-tooth should interfere, clearance {c:.5}");
}
