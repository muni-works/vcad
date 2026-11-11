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

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::thread::JoinHandle;

use serde::Serialize;
use vcad_ir::{CsgOp, Document, Node, SceneEntry};
use vcad_kernel::vcad_kernel_step::{self, StepError, StepProgress, StepStage};

use crate::err;
use crate::json::{borrow_str, guard_ptr, into_c, json_out};

#[derive(Serialize)]
struct StepImported {
    document: Document,
    /// Solids found in the file (= roots added).
    solids: usize,
    /// Faces dropped across all solids (unsupported surfaces) — the
    /// imported geometry has holes where they were.
    skipped_faces: usize,
    /// Human-readable import report when anything was dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    warnings: Option<String>,
}

/// Build an IR document importing every solid of the STEP file at `path`.
fn step_document(path: &str) -> Result<StepImported, String> {
    step_document_with(path, None)
}

/// [`step_document`], publishing progress to (and cancellable through)
/// `progress` when given.
fn step_document_with(path: &str, progress: Option<&StepProgress>) -> Result<StepImported, String> {
    let abs = std::fs::canonicalize(Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    let read = match progress {
        Some(p) => vcad_kernel_step::read_step_with_progress(&abs, p),
        None => vcad_kernel_step::read_step_with_report(&abs),
    };
    let read = match read {
        Ok(r) => r,
        Err(StepError::Cancelled) => return Err("cancelled".into()),
        Err(e) => return Err(format!("{}: {e}", abs.display())),
    };
    let solids = read.solids;
    if solids.is_empty() {
        return Err(format!("{}: no solids in file", abs.display()));
    }
    let abs_str = abs.to_string_lossy().into_owned();
    let n_solids = solids.len();
    let skipped_faces = read.report.total_skipped_faces();
    let warnings = read.report.summary();
    // Hand the parse to the evaluator: every `StepImport` node below names
    // this file, and opening the document would otherwise parse it again.
    vcad_eval::step_sources::prime_file(
        &abs_str,
        solids
            .into_iter()
            .map(vcad_kernel::Solid::from_brep)
            .collect(),
    );
    let stem = abs
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "step".into());
    let mut doc = Document::new();
    for i in 0..n_solids {
        let id = i as u64 + 1;
        let name = if n_solids == 1 {
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
        solids: n_solids,
        skipped_faces,
        warnings,
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

/// A STEP import running on its own thread (`vcad_import_step_start`).
pub struct VcadStepImportJob {
    progress: Arc<StepProgress>,
    handle: Option<JoinHandle<Result<String, String>>>,
}

/// Stage codes `vcad_import_step_poll` reports.
pub const VCAD_STEP_STAGE_READING: u32 = 0;
/// Tokenising the file (no count).
pub const VCAD_STEP_STAGE_PARSING: u32 = 1;
/// Building faces, `done` of `total`.
pub const VCAD_STEP_STAGE_FACES: u32 = 2;
/// Finished reading (the document is being assembled).
pub const VCAD_STEP_STAGE_DONE: u32 = 3;

/// Start importing the STEP file at `path` on a background thread. Poll with
/// `vcad_import_step_poll`, stop early with `vcad_import_step_cancel`, then
/// ALWAYS end with `vcad_import_step_finish` (which joins the thread and
/// frees the job). Null + `vcad_last_error` when the thread can't start.
#[no_mangle]
pub extern "C" fn vcad_import_step_start(path: *const c_char) -> *mut VcadStepImportJob {
    err::clear_error();
    let Some(p) = borrow_str("step path", path) else {
        return ptr::null_mut();
    };
    let path = p.to_owned();
    let progress = Arc::new(StepProgress::new());
    let shared = progress.clone();
    let spawned = std::thread::Builder::new()
        .name("vcad-step-import".into())
        .spawn(move || {
            match catch_unwind(AssertUnwindSafe(|| {
                step_document_with(&path, Some(&shared))
            })) {
                Ok(Ok(r)) => serde_json::to_string(&r).map_err(|e| format!("serialize: {e}")),
                Ok(Err(e)) => Err(e),
                Err(_) => Err("kernel panicked".into()),
            }
        });
    match spawned {
        Ok(handle) => Box::into_raw(Box::new(VcadStepImportJob {
            progress,
            handle: Some(handle),
        })),
        Err(e) => {
            err::set_error(format!("import step: could not start: {e}"));
            ptr::null_mut()
        }
    }
}

/// Where the import is: `stage` (`VCAD_STEP_STAGE_*`), `done` of `total`
/// (faces, for the faces stage; 0 of 0 otherwise). Returns true once the
/// thread has finished (success, failure or cancel). Any out-pointer may be
/// null.
#[no_mangle]
pub extern "C" fn vcad_import_step_poll(
    job: *const VcadStepImportJob,
    stage: *mut u32,
    done: *mut usize,
    total: *mut usize,
) -> bool {
    if job.is_null() {
        return true;
    }
    let j: &VcadStepImportJob = unsafe { &*job };
    let (s, d, t) = j.progress.snapshot();
    let code = match s {
        StepStage::Reading => VCAD_STEP_STAGE_READING,
        StepStage::Parsing => VCAD_STEP_STAGE_PARSING,
        StepStage::Faces => VCAD_STEP_STAGE_FACES,
        StepStage::Done => VCAD_STEP_STAGE_DONE,
    };
    unsafe {
        if !stage.is_null() {
            *stage = code;
        }
        if !done.is_null() {
            *done = d;
        }
        if !total.is_null() {
            *total = t;
        }
    }
    j.handle.as_ref().is_none_or(|h| h.is_finished())
}

/// Ask the import to stop (within one face). Non-blocking; still call
/// `vcad_import_step_finish`, which then fails with `"cancelled"`.
#[no_mangle]
pub extern "C" fn vcad_import_step_cancel(job: *const VcadStepImportJob) {
    if job.is_null() {
        return;
    }
    let j: &VcadStepImportJob = unsafe { &*job };
    j.progress.cancel();
}

/// Join the import thread, free the job, and return what
/// `vcad_import_step` returns (`{"document","solids","skipped_faces",
/// "warnings"?}`) or null + `vcad_last_error` (`"import step: cancelled"`
/// after a cancel). Free the string with `vcad_string_free`.
#[no_mangle]
pub extern "C" fn vcad_import_step_finish(job: *mut VcadStepImportJob) -> *mut c_char {
    if job.is_null() {
        return ptr::null_mut();
    }
    err::clear_error();
    let mut j = unsafe { Box::from_raw(job) };
    let Some(handle) = j.handle.take() else {
        return ptr::null_mut();
    };
    match handle.join() {
        Ok(Ok(json)) => into_c(json),
        Ok(Err(e)) => {
            err::set_error(format!("import step: {e}"));
            ptr::null_mut()
        }
        Err(_) => {
            err::set_error("import step: kernel panicked");
            ptr::null_mut()
        }
    }
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
        vcad_kernel::Solid::cube(10.0, 20.0, 30.0)
            .to_step(&path)
            .unwrap();

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

    fn run_job(path: &str, cancel: bool) -> (Option<serde_json::Value>, u32) {
        let c = CString::new(path).unwrap();
        let job = vcad_import_step_start(c.as_ptr());
        assert!(!job.is_null());
        if cancel {
            vcad_import_step_cancel(job);
        }
        let (mut stage, mut done, mut total) = (0u32, 0usize, 0usize);
        let mut last_stage = 0;
        while !vcad_import_step_poll(job, &mut stage, &mut done, &mut total) {
            assert!(done <= total || total == 0, "{done} of {total}");
            last_stage = last_stage.max(stage);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        vcad_import_step_poll(job, &mut stage, &mut done, &mut total);
        last_stage = last_stage.max(stage);
        let out = vcad_import_step_finish(job);
        let v = (!out.is_null()).then(|| serde_json::from_str(&take(out)).unwrap());
        (v, last_stage)
    }

    #[test]
    fn step_import_job_reports_progress_and_matches_sync() {
        let path = repo("examples/tuning-fork/tuning-fork-440hz.step");
        let (v, stage) = run_job(&path, false);
        let v = v.expect("job import");
        assert_eq!(stage, VCAD_STEP_STAGE_DONE);
        let sync = import(&path);
        assert_eq!(v["solids"], sync["solids"]);
        assert_eq!(v["skipped_faces"], sync["skipped_faces"]);
    }

    #[test]
    fn step_import_job_cancels_cleanly() {
        let path = repo("examples/tuning-fork/tuning-fork-440hz.step");
        let (v, _) = run_job(&path, true);
        assert!(v.is_none());
        let msg = crate::err::last_error_string().unwrap_or_default();
        assert!(msg.contains("cancelled"), "{msg}");
    }

    #[test]
    fn step_import_job_null_and_missing_are_safe() {
        assert!(vcad_import_step_poll(
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut()
        ));
        vcad_import_step_cancel(ptr::null());
        assert!(vcad_import_step_finish(ptr::null_mut()).is_null());
        let (v, _) = run_job("/nonexistent/x.step", false);
        assert!(v.is_none());
    }

    #[test]
    fn missing_files_fail_closed() {
        let c = CString::new("/nonexistent/x.step").unwrap();
        assert!(vcad_import_step(c.as_ptr()).is_null());
        let c = CString::new("/nonexistent/x.urdf").unwrap();
        assert!(vcad_import_urdf(c.as_ptr()).is_null());
    }
}
