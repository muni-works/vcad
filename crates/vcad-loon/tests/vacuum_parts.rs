//! Library parts from loon: `[vac]`, `[vac-at]`, `[vac-mate]` splice
//! vcad-parts geometry, pose mates onto parent ports, and derive the BOM.

use vcad_ir::{CsgOp, Document};
use vcad_loon::eval_vcad;

fn hw(doc: &Document, needle: &str) -> u32 {
    doc.hardware
        .iter()
        .filter(|l| l.spec.contains(needle))
        .map(|l| l.qty)
        .sum()
}

#[test]
fn vac_splices_library_geometry() {
    let doc = eval_vcad(
        r#"[vac "vacuum.cf.flange" #["size" "DN40" "style" "bored"]]"#,
        None,
    )
    .unwrap();
    assert!(doc.nodes.len() > 5, "flange should splice a subtree");
    assert_eq!(hw(&doc, "DN40 CF bored flange"), 1);
}

#[test]
fn mate_poses_child_on_parent_port_and_adds_joint_hardware() {
    let src = r#"
[let cube [vac "vacuum.chamber.spherical-cube" #["size" "DN100"]]]
[let vp [vac-mate "vacuum.viewport" #["size" "DN100"] "seal" cube "+y"]]
[let blank [vac-mate "vacuum.cf.flange" #["size" "DN100"] "seal" cube "-y"]]
[union cube [union vp blank]]
"#;
    let doc = eval_vcad(src, None).unwrap();
    // Two CF joints → two DN100 gaskets and 2 × 16 bolts. The cube's faces
    // are tapped, so no nuts.
    assert_eq!(hw(&doc, "DN100 CF copper gasket"), 2);
    assert_eq!(hw(&doc, "M8x"), 32);
    assert_eq!(hw(&doc, "M8 hex nut"), 0);
    assert_eq!(hw(&doc, "spherical cube"), 1);
    assert_eq!(hw(&doc, "viewport"), 1);

    // The viewport's port lands on the cube's +y face: some Translate puts
    // its origin at (0, c2f, 0).
    let info = vcad_parts::part_info(
        "vacuum.chamber.spherical-cube",
        &serde_json::from_value(serde_json::json!({"size": "DN100"})).unwrap(),
    )
    .unwrap();
    let c2f = info.metadata["center_to_face_mm"].as_f64().unwrap();
    let found = doc.nodes.values().any(|n| match &n.op {
        CsgOp::Translate { offset, .. } => {
            offset.x.abs() < 1e-6 && (offset.y - c2f).abs() < 1e-6 && offset.z.abs() < 1e-6
        }
        _ => false,
    });
    assert!(found, "no translate to the +y port at {c2f}");
}

#[test]
fn mate_chain_reducer_then_kf_adapter() {
    let src = r#"
[let cube [vac "vacuum.chamber.spherical-cube" #["size" "DN100"]]]
[let red [vac-mate "vacuum.cf.reducer" #["large" "DN100" "small" "DN40"] "large" cube "-z"]]
[let ad [vac-mate "vacuum.adapter.cf-kf" #["cf" "DN40" "kf" "KF40" "length" "60"] "cf" red "small"]]
[let g [vac-mate "vacuum.gauge" #["size" "KF40"] "inlet" ad "kf"]]
[union cube [union red [union ad g]]]
"#;
    let doc = eval_vcad(src, None).unwrap();
    assert_eq!(hw(&doc, "DN100 CF copper gasket"), 1);
    assert_eq!(hw(&doc, "DN40 CF copper gasket"), 1);
    // The reducer's small side is tapped: bolts but no nuts on that joint.
    assert_eq!(hw(&doc, "M6 hex nut"), 0);
    assert_eq!(hw(&doc, "M6x"), 6);
    assert_eq!(hw(&doc, "KF40 centering ring"), 1);
    assert_eq!(hw(&doc, "KF40 wing-nut clamp"), 1);
}

#[test]
fn numeric_param_values_are_accepted() {
    let doc = eval_vcad(
        r#"[vac "vacuum.nipple" #["size" "DN40" "length" 150]]"#,
        None,
    );
    // Whether loon admits a number in a (Vec Str) or not, the result must be
    // either a 150 mm nipple or a clear type error — never a silent default.
    match doc {
        Ok(d) => assert_eq!(hw(&d, "150 mm"), 1, "{:?}", d.hardware),
        Err(e) => assert!(!e.is_empty()),
    }
}

#[test]
fn mismatched_ports_are_refused() {
    let src = r#"
[let cube [vac "vacuum.chamber.spherical-cube" #["size" "DN100"]]]
[vac-mate "vacuum.viewport" #["size" "DN40"] "seal" cube "+x"]
"#;
    let err = eval_vcad(src, None).unwrap_err();
    assert!(err.contains("reducer"), "{err}");
}

#[test]
fn unknown_port_names_the_choices() {
    let src = r#"
[let cube [vac "vacuum.chamber.spherical-cube" #["size" "DN100"]]]
[vac-mate "vacuum.viewport" #["size" "DN100"] "seal" cube "top"]
"#;
    let err = eval_vcad(src, None).unwrap_err();
    assert!(err.contains("+z"), "{err}");
}

#[test]
fn vac_at_pose_composes_into_mates() {
    // Cube rotated 90° about Z: its +x port now faces +y.
    let src = r#"
[let cube [vac-at "vacuum.chamber.spherical-cube" #["size" "DN100"] 0 0 500 0 0 90]]
[let n [vac-mate "vacuum.nipple" #["size" "DN100" "length" "100"] "-z" cube "+x"]]
[union cube n]
"#;
    let doc = eval_vcad(src, None).unwrap();
    let info = vcad_parts::part_info(
        "vacuum.chamber.spherical-cube",
        &serde_json::from_value(serde_json::json!({"size": "DN100"})).unwrap(),
    )
    .unwrap();
    let c2f = info.metadata["center_to_face_mm"].as_f64().unwrap();
    let found = doc.nodes.values().any(|n| match &n.op {
        CsgOp::Translate { offset, .. } => {
            offset.x.abs() < 1e-6
                && (offset.y - c2f).abs() < 1e-6
                && (offset.z - 500.0).abs() < 1e-6
        }
        _ => false,
    });
    assert!(found, "nipple not placed at the rotated +x port");
}
