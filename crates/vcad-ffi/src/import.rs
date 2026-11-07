//! File import into IR documents (`vcad_import_*`).
//!
//! STEP becomes `StepImport` nodes (one root per solid in the file), not baked
//! meshes: the document stays small and the geometry stays B-rep, so imported
//! faces survive fillets, booleans, picking and STEP re-export. The file is
//! parsed once here to count solids and fail early on unreadable input; the
//! evaluator reads it again by path when the document is evaluated, so the
//! path must stay valid (copy the file next to the document first if needed).
//!
//! URDF becomes a full assembly document (part defs, instances, joints) via
//! `vcad-kernel-urdf`.

use std::ffi::c_char;
use std::path::Path;
use std::ptr;

use serde::Serialize;
use vcad_ir::{CsgOp, Document, Node, SceneEntry};
use vcad_kernel::Solid;

use crate::err;
use crate::json::{borrow_str, guard_ptr, json_out};

#[derive(Serialize)]
struct StepImported {
    document: Document,
    /// Solids found in the file (= roots added).
    solids: usize,
}

/// Build an IR document importing every solid of the STEP file at `path`.
fn step_document(path: &str) -> Result<StepImported, String> {
    let abs = std::fs::canonicalize(Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    let solids = Solid::from_step_all(&abs).map_err(|e| format!("{}: {e}", abs.display()))?;
    if solids.is_empty() {
        return Err(format!("{}: no solids in file", abs.display()));
    }
    let abs_str = abs.to_string_lossy().into_owned();
    let stem = abs
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "step".into());
    let mut doc = Document::new();
    for i in 0..solids.len() {
        let id = i as u64 + 1;
        let name = if solids.len() == 1 {
            stem.clone()
        } else {
            format!("{stem} #{}", i + 1)
        };
        doc.nodes.insert(
            id,
            Node {
                id,
                name: Some(name),
                op: CsgOp::StepImport {
                    path: abs_str.clone(),
                    solid_index: Some(i as u32),
                },
            },
        );
        doc.roots.push(SceneEntry {
            root: id,
            material: "default".into(),
            visible: None,
        });
    }
    Ok(StepImported {
        document: doc,
        solids: solids.len(),
    })
}

/// Import a STEP (AP203/AP214) file as an IR document.
///
/// `path`: UTF-8, NUL-terminated. Returns `{"document": <Document>,
/// "solids": N}` or null + `vcad_last_error` (unreadable file, no solids).
/// Evaluate with `vcad_scene_from_json`; to merge into an open document,
/// copy the `StepImport` nodes across (renumbering ids). Free with
/// `vcad_string_free`.
#[no_mangle]
pub extern "C" fn vcad_import_step(path: *const c_char) -> *mut c_char {
    err::clear_error();
    let Some(p) = borrow_str("step path", path) else {
        return ptr::null_mut();
    };
    guard_ptr("import step", || match step_document(p) {
        Ok(r) => json_out(&r),
        Err(e) => {
            err::set_error(format!("import step: {e}"));
            ptr::null_mut()
        }
    })
}

/// Import a URDF robot description as an IR assembly document (JSON).
/// Mesh references resolve relative to the URDF's directory. Null +
/// `vcad_last_error` on failure. Free with `vcad_string_free`.
#[no_mangle]
pub extern "C" fn vcad_import_urdf(path: *const c_char) -> *mut c_char {
    err::clear_error();
    let Some(p) = borrow_str("urdf path", path) else {
        return ptr::null_mut();
    };
    guard_ptr("import urdf", || match vcad_kernel_urdf::read_urdf(p) {
        Ok(doc) => json_out(&doc),
        Err(e) => {
            err::set_error(format!("import urdf: {e}"));
            ptr::null_mut()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::take;
    use std::ffi::CString;
    use vcad_eval::{evaluate_document, EvalOptions};

    fn repo(rel: &str) -> String {
        format!("{}/../../{rel}", env!("CARGO_MANIFEST_DIR"))
    }

    fn import(path: &str) -> serde_json::Value {
        let c = CString::new(path).unwrap();
        serde_json::from_str(&take(vcad_import_step(c.as_ptr()))).unwrap()
    }

    #[test]
    fn import_a_step_cube_round_trip() {
        let dir = std::env::temp_dir().join(format!("vcad-ffi-step-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cube.step");
        Solid::cube(10.0, 20.0, 30.0).to_step(&path).unwrap();

        let r = import(path.to_str().unwrap());
        assert_eq!(r["solids"], 1);
        let doc = Document::from_json(&r["document"].to_string()).unwrap();
        let scene = evaluate_document(&doc, &EvalOptions::default()).unwrap();
        assert!(scene.failures.is_empty(), "{:?}", scene.failures);
        let v = scene.parts[0].solid.as_ref().unwrap().volume();
        assert!((v - 6000.0).abs() < 1.0, "volume {v}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_the_tuning_fork_sample() {
        let r = import(&repo("examples/tuning-fork/tuning-fork-440hz.step"));
        assert!(r["solids"].as_u64().unwrap() >= 1);
        let doc = Document::from_json(&r["document"].to_string()).unwrap();
        let scene = evaluate_document(&doc, &EvalOptions::default()).unwrap();
        assert!(scene.failures.is_empty(), "{:?}", scene.failures);
        assert!(scene.parts[0].mesh.indices.len() > 30);
    }

    #[test]
    fn import_urdf_assembly() {
        let c = CString::new(repo("examples/robot-arm-2dof.urdf")).unwrap();
        let doc = Document::from_json(&take(vcad_import_urdf(c.as_ptr()))).unwrap();
        assert!(doc.joints.as_ref().is_some_and(|j| !j.is_empty()));
    }

    #[test]
    fn missing_files_fail_closed() {
        let c = CString::new("/nonexistent/x.step").unwrap();
        assert!(vcad_import_step(c.as_ptr()).is_null());
        let c = CString::new("/nonexistent/x.urdf").unwrap();
        assert!(vcad_import_urdf(c.as_ptr()).is_null());
    }
}
