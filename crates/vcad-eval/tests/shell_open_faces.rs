//! `Shell.open_faces` through the document evaluator: old documents (no
//! `open_faces`) evaluate exactly as before, and an open face — by
//! persistent name or by geometric hint — leaves the opening.

use vcad_kernel::Solid;

fn doc(shell: serde_json::Value) -> vcad_ir::Document {
    serde_json::from_value(serde_json::json!({
        "version": "0.1",
        "nodes": {
            "1": { "id": 1, "name": "box",
                   "op": { "type": "Cube", "size": { "x": 90.0, "y": 70.0, "z": 25.0 } } },
            "2": { "id": 2, "name": "shell", "op": shell }
        },
        "materials": {},
        "part_materials": {},
        "roots": [{ "root": 2, "material": "default" }]
    }))
    .expect("document parses")
}

fn volume(d: &vcad_ir::Document) -> f64 {
    let roots = vcad_eval::evaluate_root_solids(d).expect("document evaluates");
    roots[0].solid.as_ref().expect("a solid").volume()
}

#[test]
fn old_document_without_open_faces_is_unchanged() {
    let d = doc(serde_json::json!({ "type": "Shell", "child": 1, "thickness": 2.0 }));
    match &d.nodes[&2].op {
        vcad_ir::CsgOp::Shell { open_faces, .. } => assert!(open_faces.is_empty()),
        other => panic!("{other:?}"),
    }
    // Round-trips without growing an `open_faces` key.
    let json = serde_json::to_value(&d.nodes[&2].op).unwrap();
    assert!(json.get("open_faces").is_none(), "{json}");
    // Same solid as the closed-shell kernel call.
    let expected = Solid::cube(90.0, 70.0, 25.0).shell(2.0).unwrap().volume();
    assert_eq!(volume(&d), expected);
}

#[test]
fn open_face_by_name_and_by_hint() {
    let expected = 90.0 * 70.0 * 25.0 - 86.0 * 66.0 * 23.0;
    for face in [
        serde_json::json!({ "name": "n1:top" }),
        serde_json::json!({ "point": { "x": 45.0, "y": 35.0, "z": 25.0 },
                            "normal": { "x": 0.0, "y": 0.0, "z": 1.0 } }),
        // A lost name falls back to the hint.
        serde_json::json!({ "name": "n1:gone",
                            "point": { "x": 45.0, "y": 35.0, "z": 25.0 } }),
    ] {
        let d = doc(serde_json::json!({
            "type": "Shell", "child": 1, "thickness": 2.0, "open_faces": [face]
        }));
        let v = volume(&d);
        assert!(
            (v - expected).abs() < 1e-6 * expected,
            "{face}: {v} vs {expected}"
        );
    }
}

#[test]
fn consuming_thickness_is_an_evaluation_error() {
    let d = doc(serde_json::json!({
        "type": "Shell", "child": 1, "thickness": 30.0,
        "open_faces": [{ "name": "n1:top" }]
    }));
    assert!(vcad_eval::evaluate_root_solids(&d).is_err());
}
