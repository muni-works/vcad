//! Gradient-driven optimize: converges on a toy part, FD-checked gradient,
//! sensitivity field with the right sign.
mod common;

use serde_json::json;
use vcad_atoms::optimize::face_sensitivities;
use vcad_atoms::task::TaskStatus;
use vcad_kernel_tessellate::TessellationParams;

#[test]
fn minimize_mass_with_height_floor_converges() {
    // min mass of a cylinder, r ∈ [3, 15], h ∈ [2, 30], subject to bbox_z ≥ 20.
    // Optimum: r = 3 (lower bound), h = 20 (active constraint).
    let req = json!({
        "parameters": [{"name": "r", "min": 3, "max": 15}, {"name": "h", "min": 2, "max": 30}],
        "objective": {"qoi": "mass", "sense": "min"},
        "constraints": [{"qoi": "bbox_z", "op": ">=", "value": 20}],
        "maxIterations": 60, "tolerance": 1e-3
    });
    let h =
        vcad_atoms::spawn_tool("optimize_parameters", common::cylinder(10.0, 25.0), req).unwrap();
    let s = h.wait();
    assert_eq!(
        s.status,
        TaskStatus::Completed,
        "{:?} {:?}",
        s.error,
        s.result
    );
    let r = s.result.unwrap();
    let r_opt = r["parameters"][0]["value"].as_f64().unwrap();
    let h_opt = r["parameters"][1]["value"].as_f64().unwrap();
    assert!((r_opt - 3.0).abs() < 1e-6, "r = {r_opt}");
    assert!((h_opt - 20.0).abs() < 0.05, "h = {h_opt}");
    assert_eq!(r["fdCheck"]["pass"], true, "{}", r["fdCheck"]);
    for p in r["fdCheck"]["perParameter"].as_array().unwrap() {
        assert!(p["relError"].as_f64().unwrap() < 5e-3, "{p}");
    }
    assert!(
        r["objective"]["final"].as_f64().unwrap() < 0.1 * r["objective"]["start"].as_f64().unwrap()
    );
}

#[test]
fn stiffness_goal_is_refused() {
    let req = json!({"parameters": [{"name": "r", "min": 3, "max": 15}],
                     "constraints": [{"qoi": "stiffness", "op": ">=", "value": 1}]});
    let s = vcad_atoms::spawn_tool("optimize_parameters", common::cylinder(5.0, 5.0), req)
        .unwrap()
        .wait();
    assert_eq!(s.status, TaskStatus::Failed);
    assert!(s.error.unwrap().contains("FEA"));
}

#[test]
fn optimize_cancel() {
    let req = json!({"parameters": [{"name": "r", "min": 3, "max": 15}], "maxIterations": 200});
    let h =
        vcad_atoms::spawn_tool("optimize_parameters", common::cylinder(14.0, 5.0), req).unwrap();
    h.cancel();
    assert_eq!(h.wait().status, TaskStatus::Cancelled);
}

#[test]
fn radius_sensitivity_moves_the_wall_outward() {
    let tess = TessellationParams {
        circle_segments: 48,
        height_segments: 2,
        ..Default::default()
    };
    let f = face_sensitivities(&common::cylinder(10.0, 8.0), "r", &tess, 1e-4).unwrap();
    let part = &f["parts"][0];
    let vn: Vec<f64> = part["triNormalVelocity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(vn.len() * 3, part["indices"].as_array().unwrap().len());
    // Wall triangles move outward at ~1 mm/mm (cos of the half-facet angle);
    // cap triangles move tangentially (≈0). Nothing moves inward.
    let max = vn.iter().cloned().fold(f64::MIN, f64::max);
    assert!((max - 1.0).abs() < 0.02, "max v_n = {max}");
    assert!(vn.iter().all(|v| *v > -1e-6), "inward motion");
    // Height sensitivity: only the top cap moves (outward, 1 mm/mm).
    let fh = face_sensitivities(&common::cylinder(10.0, 8.0), "h", &tess, 1e-4).unwrap();
    let vh: Vec<f64> = fh["parts"][0]["triNormalVelocity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let mx = vh.iter().cloned().fold(f64::MIN, f64::max);
    assert!((mx - 1.0).abs() < 1e-6, "top cap v_n = {mx}");
}
