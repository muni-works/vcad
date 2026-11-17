//! A kernel panic while evaluating a document must come back through the C
//! ABI as a failure of that one root, never as a dead process.
//!
//! vcad.app 1.0.3-beta.5 shipped linked with Rust's `panic = "abort"` runtime
//! (it came in with the second Rust staticlib, `libtang_compress_ffi.a`), so
//! every `catch_unwind` in vcad-eval and vcad-ffi was dead code and one
//! tessellation assertion in `fusor.vcad` was SIGABRT for the whole app.
//!
//! What this test covers: the catch path itself, in a binary built with the
//! workspace profile. A root that panics (vcad-eval's `panic-injection` hook)
//! is reported as a `kernel panic: …` failure, its neighbour still meshes,
//! and `vcad_scene_from_json` hands back a usable scene. If the workspace
//! profile ever flips to `panic = "abort"`, this test binary aborts.
//!
//! What it cannot cover: the app's final link, where the two staticlibs'
//! panic runtimes meet. That is `apple/VcadApp/scripts/check-unwind.sh`, run
//! by build-ffi.sh, build-tc-ffi.sh, bundle.sh and release.sh.

use vcad_eval::{evaluate_document_with_sheet_metal, EvalOptions, PANIC_INJECTION_NODE};
use vcad_ffi::{
    vcad_scene_free, vcad_scene_from_json, vcad_scene_part_count, vcad_scene_part_mesh,
};
use vcad_ir::Document;

/// Two cube roots; the second one carries the injection name.
fn document_with_a_panicking_root() -> String {
    let mut doc = serde_json::to_value(Document::new()).unwrap();
    let cube = |id: u64, name: &str| {
        serde_json::json!({
            "id": id,
            "name": name,
            "op": { "type": "Cube", "size": { "x": 10.0, "y": 10.0, "z": 10.0 } },
        })
    };
    doc["nodes"] = serde_json::json!({
        "1": cube(1, "good"),
        "2": cube(2, PANIC_INJECTION_NODE),
    });
    doc["roots"] = serde_json::json!([
        { "root": 1, "material": "default" },
        { "root": 2, "material": "default" },
    ]);
    doc.to_string()
}

#[test]
fn a_panicking_root_is_a_reported_failure_in_the_evaluator() {
    let doc = Document::from_json(&document_with_a_panicking_root()).unwrap();
    let scene = evaluate_document_with_sheet_metal(&doc, &EvalOptions::default())
        .expect("a panicking root must not fail the whole document");

    assert_eq!(scene.failures.len(), 1, "{:?}", scene.failures);
    let failure = &scene.failures[0];
    assert_eq!(failure.node_id, 2);
    assert!(
        failure.error.starts_with("kernel panic:") && failure.error.contains("panic-injection"),
        "expected the caught panic's message, got {:?}",
        failure.error
    );
}

#[test]
fn a_panicking_root_comes_back_through_the_c_abi_as_a_scene() {
    let json = document_with_a_panicking_root();
    let scene = vcad_scene_from_json(json.as_ptr(), json.len());
    assert!(
        !scene.is_null(),
        "vcad_scene_from_json returned null: one panicking root took the document down"
    );

    let triangles: Vec<usize> = (0..vcad_scene_part_count(scene))
        .map(|i| vcad_scene_part_mesh(scene, i).indices_len / 3)
        .collect();
    vcad_scene_free(scene);

    // The good cube still has geometry; the panicking root contributes none.
    assert_eq!(
        triangles.iter().filter(|&&t| t > 0).count(),
        1,
        "per-part triangle counts: {triangles:?}"
    );
    assert_eq!(triangles.iter().sum::<usize>(), 12, "{triangles:?}");
}
