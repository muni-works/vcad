//! Time opening a STEP assembly the way vcad.app does (import → viewport
//! evaluation), phase by phase. `cargo run --release -p vcad-eval --example
//! assembly_open_bench -- file.step` (Voron: `scripts/fetch-voron-step.sh`).
//! `CLASH=1` runs clash detection like the old open path did. The root mesh
//! cache is a fresh (cold) directory per run, as on a first open in the app;
//! `CACHE_DIR=<dir>` reuses one (a warm reopen), `NOCACHE=1` turns it off.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: assembly_open_bench <file.step>");
    let t0 = Instant::now();
    let read = vcad_kernel_step::read_step_with_report(std::path::Path::new(&path)).expect("read");
    let parse_s = t0.elapsed().as_secs_f64();
    let doc = vcad_eval::step_doc::step_assembly_document(&read, &path, "bench");
    let n_solids = read.solids.len();
    // Seed the evaluator's per-file parse cache, as the app's importer does.
    let solids = read.solids.clone();
    println!(
        "parse {:.2}s: {} solids, {} roots",
        parse_s,
        read.solids.len(),
        doc.roots.len()
    );
    drop(read);
    vcad_eval::step_sources::prime_file(
        &path,
        solids
            .into_iter()
            .map(vcad_kernel::Solid::from_brep)
            .collect(),
    );
    let tk = Instant::now();
    let keys: std::collections::HashSet<_> = (0..n_solids)
        .filter_map(|i| vcad_eval::step_sources::solid_key(&path, i))
        .collect();
    println!(
        "content keys {:.2}s: {} distinct shapes of {}",
        tk.elapsed().as_secs_f64(),
        keys.len(),
        n_solids
    );
    let first = Arc::new(std::sync::Mutex::new(None::<f64>));
    let landed = Arc::new(AtomicUsize::new(0));
    let t1 = Instant::now();
    let (f, l) = (first.clone(), landed.clone());
    let opts = vcad_eval::EvalOptions {
        skip_clash_detection: std::env::var("CLASH").is_err(),
        on_root: Some(Box::new(move |_i, _n, _m| {
            l.fetch_add(1, Ordering::Relaxed);
            f.lock().unwrap().get_or_insert(t1.elapsed().as_secs_f64());
        })),
        root_cache: cache(),
        ..Default::default()
    };
    let _chord = vcad_eval::evaluate::DisplayChordGuard::enable(
        std::env::var("CHORD")
            .ok()
            .and_then(|c| c.parse().ok())
            .unwrap_or(0.02),
    );
    let _preview =
        vcad_eval::evaluate::DisplayPreviewGuard::enable(std::env::var("PREVIEW").is_ok());
    let scene = vcad_eval::evaluate_document(&doc, &opts).expect("evaluate");
    let empty = scene
        .parts
        .iter()
        .filter(|p| p.mesh.indices.is_empty())
        .count();
    println!(
        "{} failed roots, {} empty parts",
        scene.failures.len(),
        empty
    );
    for f in scene.failures.iter().take(10) {
        println!("  fail {}: {}", f.scope, f.error);
    }
    let tris: usize = scene.parts.iter().map(|p| p.mesh.indices.len() / 3).sum();
    println!(
        "evaluate {:.2}s (first part {:.2}s), {} parts, {} tris, {} clashes",
        t1.elapsed().as_secs_f64(),
        first.lock().unwrap().unwrap_or(f64::NAN),
        scene.parts.len(),
        tris,
        scene.clashes.len()
    );
}

fn cache() -> Option<std::rc::Rc<dyn vcad_eval::cache::RootMeshCache>> {
    if std::env::var("NOCACHE").is_ok() {
        return None;
    }
    let dir = std::env::var("CACHE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::temp_dir().join(format!("vcad-asm-bench-{}", std::process::id()))
        });
    let c = vcad_eval::cache::DiskMeshCache::at(dir)?;
    Some(std::rc::Rc::new(c))
}
