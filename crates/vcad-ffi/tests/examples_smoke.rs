//! Every example document in `examples/` opens the way the native app opens
//! it: any `.vcad` format (JSON, loon, VCode) loaded through
//! `vcad_ffi::load_document_file` (mesh paths resolved against the file's
//! directory), opened in the document engine, then evaluated with the app's
//! kernel-direct evaluator, without a panic and with non-empty geometry.
//!
//! New examples are picked up automatically; a file that can't meet the bar
//! fails here instead of in front of a user on the Open Example menu.
//!
//! Each evaluation runs against [`BUDGET`]: an example that blows it fails
//! *by name* instead of silently eating the CI shard's 45-minute limit.
//! Examples whose evaluation is known to take many minutes in the current
//! boolean kernel are listed in [`HEAVY`]: the default test still loads them
//! and opens them in the engine, and the ignored
//! `heavy_examples_evaluate` (run nightly) evaluates them in full.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use vcad_app::DocumentApi;
use vcad_crdt::ReplicaId;
use vcad_eval::{evaluate_document_with_sheet_metal, EvalOptions};
use vcad_ir::Document;

/// Examples (repo-relative) whose evaluation takes tens of minutes in the
/// current boolean kernel. Keep this list short and say why.
///
/// - `fusor-chamber.loon`: nine `std:vacuum.*` roots. It took 1067–2428 s on
///   CI while the DN100 spherical cube's port bores were mis-rotated into
///   each other; that is fixed and the cube is now ~7 s, but the whole
///   example is still ~90 s in the test profile on a 24-core box (one
///   other root falls back to the mesh boolean), too close to [`BUDGET`]
///   for a CI runner. Un-list it once that root stays analytic.
const HEAVY: &[&str] = &["examples/vacuum/fusor-chamber.loon"];

/// Wall-clock budget for one non-heavy example (load + engine open +
/// evaluate). Every non-heavy example takes ~0.1–12 s; the budget is
/// generous so a loaded runner never trips it, yet a hang or a new
/// minutes-long example fails here with its name.
const BUDGET: Duration = Duration::from_secs(300);

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rel(path: &Path) -> String {
    path.strip_prefix(repo())
        .unwrap_or(path)
        .display()
        .to_string()
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("vcad" | "loon")
        ) {
            out.push(path);
        }
    }
}

fn examples() -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect(&repo().join("examples"), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no examples found");
    files
}

/// The app's Open: any .vcad format (JSON, loon, VCode) to IR, then into
/// the engine.
fn load(path: &Path) -> Result<Document, String> {
    let doc = vcad_ffi::load_document_file(path)?;
    // The editor opens documents in the engine; a refusal there means a
    // view-only document, which an example must not be. Electronics
    // documents are edited outside the engine by design.
    let electronics = doc.schematic.is_some() || doc.pcb.is_some();
    if !electronics {
        let json = serde_json::to_string(&doc).map_err(|e| format!("serialize: {e}"))?;
        DocumentApi::from_ir(&json, ReplicaId(1)).map_err(|e| format!("engine open: {e}"))?;
    }
    Ok(doc)
}

/// Evaluate a loaded example; `Ok(triangles)` or a reason.
fn evaluate(doc: &Document) -> Result<usize, String> {
    let scene = evaluate_document_with_sheet_metal(doc, &EvalOptions::default())
        .map_err(|e| format!("evaluate: {e}"))?;
    let parts: usize = scene.parts.iter().map(|p| p.mesh.indices.len() / 3).sum();
    let instances: usize = scene
        .instances
        .iter()
        .flatten()
        .map(|i| i.mesh.indices.len() / 3)
        .sum();
    let triangles = parts + instances;
    if triangles == 0 {
        return Err(format!(
            "no geometry ({} root failure(s): {:?})",
            scene.failures.len(),
            scene.failures.iter().take(3).collect::<Vec<_>>()
        ));
    }
    Ok(triangles)
}

fn caught<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err("panicked".into()))
}

/// Load + evaluate on a worker thread, bounded by [`BUDGET`]. `None` means
/// the budget ran out; the worker is abandoned (the test process exits when
/// the test fails).
fn open_within_budget(path: &Path) -> Option<Result<usize, String>> {
    let (tx, rx) = mpsc::channel();
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(caught(|| evaluate(&load(&path)?)));
    });
    rx.recv_timeout(BUDGET).ok()
}

#[test]
fn heavy_list_names_real_examples() {
    let files: Vec<String> = examples().iter().map(|p| rel(p)).collect();
    for heavy in HEAVY {
        assert!(
            files.iter().any(|f| f == heavy),
            "HEAVY lists {heavy}, which is not an example any more"
        );
    }
}

#[test]
fn every_example_opens_with_geometry() {
    let files = examples();
    let mut failures = Vec::new();
    for path in &files {
        let rel = rel(path);
        let started = Instant::now();
        if HEAVY.contains(&rel.as_str()) {
            // Still opened exactly like the app does; full evaluation is in
            // `heavy_examples_evaluate`.
            match caught(|| load(path)) {
                Ok(_) => eprintln!("open {rel}: heavy, loaded in {:?}", started.elapsed()),
                Err(e) => failures.push(format!("{rel}: {e}")),
            }
            continue;
        }
        match open_within_budget(path) {
            Some(Ok(tris)) => eprintln!("ok   {rel}: {tris} triangles in {:?}", started.elapsed()),
            Some(Err(e)) => failures.push(format!("{rel}: {e}")),
            None => {
                failures.push(format!(
                    "{rel}: still evaluating after {BUDGET:?} — fix the slow path, or \
                     list it in HEAVY with a reason"
                ));
                // The abandoned worker would compete with every later
                // example; stop here.
                break;
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} examples failed:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}

#[test]
#[ignore = "tens of minutes of boolean time; runs nightly (ci.yml heavy-examples)"]
fn heavy_examples_evaluate() {
    let mut failures = Vec::new();
    for heavy in HEAVY {
        let path = repo().join(heavy);
        let started = Instant::now();
        match caught(|| evaluate(&load(&path)?)) {
            Ok(tris) => eprintln!("ok   {heavy}: {tris} triangles in {:?}", started.elapsed()),
            Err(e) => failures.push(format!("{heavy}: {e}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
