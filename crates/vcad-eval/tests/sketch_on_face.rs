//! `SketchOnFace`: a sketch attached to a named face follows that face when
//! upstream geometry changes, and fails closed when the face is gone.
//!
//! The document is a plate (Sketch2D + Extrude) with a boss sketched on its
//! top cap (`n2:end`, the extrude's far cap) and extruded up. Making the plate
//! thicker must carry the boss up with it; a frozen sketch plane would leave
//! the boss buried inside the plate.

fn rect(w: f64, d: f64) -> serde_json::Value {
    let p = |x: f64, y: f64| serde_json::json!({ "x": x, "y": y });
    let line = |a, b| serde_json::json!({ "type": "Line", "start": a, "end": b });
    serde_json::json!([
        line(p(0.0, 0.0), p(w, 0.0)),
        line(p(w, 0.0), p(w, d)),
        line(p(w, d), p(0.0, d)),
        line(p(0.0, d), p(0.0, 0.0)),
    ])
}

fn doc(thickness: f64, face: serde_json::Value) -> vcad_ir::Document {
    let v = |x: f64, y: f64, z: f64| serde_json::json!({ "x": x, "y": y, "z": z });
    serde_json::from_value(serde_json::json!({
        "version": "0.1",
        "nodes": {
            "1": { "id": 1, "name": "plate sketch", "op": { "type": "Sketch2D",
                "origin": v(0.0, 0.0, 0.0), "x_dir": v(1.0, 0.0, 0.0), "y_dir": v(0.0, 1.0, 0.0),
                "segments": rect(80.0, 50.0) } },
            "2": { "id": 2, "name": "plate", "op": { "type": "Extrude", "sketch": 1,
                "direction": v(0.0, 0.0, thickness) } },
            // Authored when the plate was 8 mm thick: origin at z = 8.
            "3": { "id": 3, "name": "boss sketch", "op": { "type": "Sketch2D",
                "origin": v(30.0, 15.0, 8.0), "x_dir": v(1.0, 0.0, 0.0), "y_dir": v(0.0, 1.0, 0.0),
                "segments": rect(20.0, 20.0) } },
            "4": { "id": 4, "name": "on top", "op": { "type": "SketchOnFace",
                "sketch": 3, "target": 2, "face": face } },
            "5": { "id": 5, "name": "boss", "op": { "type": "Extrude", "sketch": 4,
                "direction": v(0.0, 0.0, 20.0) } }
        },
        "materials": {},
        "part_materials": {},
        "roots": [{ "root": 5, "material": "default" }]
    }))
    .expect("document parses")
}

fn boss_z(d: &vcad_ir::Document) -> (f64, f64) {
    let roots = vcad_eval::evaluate_root_solids(d).expect("document evaluates");
    let (min, max) = roots[0].solid.as_ref().expect("a solid").bounding_box();
    (min[2], max[2])
}

#[test]
fn boss_follows_the_plate_top_when_the_plate_gets_thicker() {
    for t in [8.0, 12.0, 3.0] {
        let (lo, hi) = boss_z(&doc(t, serde_json::json!({ "name": "n2:end" })));
        assert!((lo - t).abs() < 1e-9, "t={t}: boss starts at z={lo}");
        assert!((hi - (t + 20.0)).abs() < 1e-9, "t={t}: boss ends at z={hi}");
    }
}

#[test]
fn in_plane_placement_is_kept() {
    let roots = vcad_eval::evaluate_root_solids(&doc(12.0, serde_json::json!({ "name": "n2:end" })))
        .unwrap();
    let (min, max) = roots[0].solid.as_ref().unwrap().bounding_box();
    assert!((min[0] - 30.0).abs() < 1e-9 && (max[0] - 50.0).abs() < 1e-9, "{min:?} {max:?}");
    assert!((min[1] - 15.0).abs() < 1e-9 && (max[1] - 35.0).abs() < 1e-9, "{min:?} {max:?}");
}

#[test]
fn a_face_that_does_not_resolve_fails_closed() {
    let r = vcad_eval::evaluate_root_solids(&doc(8.0, serde_json::json!({ "name": "n2:nope" })));
    let msg = match r {
        Err(e) => e.to_string(),
        Ok(roots) => panic!("must not evaluate; got solid={:?}", roots[0].solid.is_some()),
    };
    assert!(msg.contains("sketch on face"), "{msg}");
}
