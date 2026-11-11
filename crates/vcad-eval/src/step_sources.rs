//! In-memory registry of STEP file contents, keyed by the path a
//! [`CsgOp::StepImport`](vcad_ir::CsgOp::StepImport) node stores.
//!
//! A `StepImport` node is a *lazy reference*: the document keeps a path and
//! the evaluator opens the file. That works natively (CLI, desktop), but the
//! WASM kernel — which is what the MCP server and the browser run — has no
//! filesystem, so the same node would evaluate to nothing at all.
//!
//! Registering the bytes under the node's path closes that gap: the evaluator
//! prefers a registered source and only falls back to the filesystem. The
//! caller that *has* the bytes (an MCP import, a browser drag-drop) registers
//! them once; every later evaluation of the document resolves B-rep, so
//! analytic faces survive into booleans, fillets, and STEP export.
//!
//! Parsed solids are cached alongside the bytes — a vendor assembly is
//! megabytes of STEP text, and a document is re-evaluated on every edit.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use vcad_kernel::Solid;

/// A registered STEP source: the raw bytes plus the solids parsed from them.
struct StepSource {
    bytes: Arc<Vec<u8>>,
    /// Parsed lazily on first evaluation, then reused.
    solids: Option<Arc<Vec<Solid>>>,
}

fn registry() -> &'static Mutex<HashMap<String, StepSource>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, StepSource>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register STEP file contents under `path`.
///
/// `path` must match the string stored in the `StepImport` node verbatim.
/// Re-registering the same path replaces the bytes and drops the parse cache.
pub fn register(path: impl Into<String>, bytes: Vec<u8>) {
    let mut reg = registry().lock().expect("step source registry poisoned");
    reg.insert(
        path.into(),
        StepSource {
            bytes: Arc::new(bytes),
            solids: None,
        },
    );
}

/// Register STEP contents together with an already-parsed solid list.
///
/// The importer parses the file anyway to report skipped faces; handing the
/// result over here means the first evaluation doesn't re-parse megabytes.
pub fn register_parsed(path: impl Into<String>, bytes: Vec<u8>, solids: Vec<Solid>) {
    let mut reg = registry().lock().expect("step source registry poisoned");
    reg.insert(
        path.into(),
        StepSource {
            bytes: Arc::new(bytes),
            solids: Some(Arc::new(solids)),
        },
    );
}

/// Whether `path` has registered contents.
pub fn is_registered(path: &str) -> bool {
    registry()
        .lock()
        .expect("step source registry poisoned")
        .contains_key(path)
}

/// Raw registered bytes for `path`, if any.
pub fn bytes(path: &str) -> Option<Arc<Vec<u8>>> {
    registry()
        .lock()
        .expect("step source registry poisoned")
        .get(path)
        .map(|s| s.bytes.clone())
}

/// Forget the source registered under `path`.
pub fn unregister(path: &str) {
    registry()
        .lock()
        .expect("step source registry poisoned")
        .remove(path);
}

/// Forget every registered source.
pub fn clear() {
    registry()
        .lock()
        .expect("step source registry poisoned")
        .clear();
}

/// Parse (or reuse the cached parse of) the solids registered under `path`.
///
/// Returns `Ok(None)` when nothing is registered for `path` — the caller
/// falls back to the filesystem. A parse failure is an error, never a silent
/// empty result.
pub fn solids(path: &str) -> Result<Option<Arc<Vec<Solid>>>, String> {
    let bytes = {
        let reg = registry().lock().expect("step source registry poisoned");
        match reg.get(path) {
            None => return Ok(None),
            Some(src) => {
                if let Some(cached) = &src.solids {
                    return Ok(Some(cached.clone()));
                }
                src.bytes.clone()
            }
        }
    };

    // Parse outside the lock: a large assembly takes seconds, and holding the
    // registry mutex across it would serialize unrelated evaluations.
    let parsed = Solid::from_step_buffer_all(&bytes).map_err(|e| e.to_string())?;
    let parsed = Arc::new(parsed);

    let mut reg = registry().lock().expect("step source registry poisoned");
    if let Some(src) = reg.get_mut(path) {
        // Only cache if the entry still holds the bytes we parsed.
        if Arc::ptr_eq(&src.bytes, &bytes) {
            src.solids = Some(parsed.clone());
        }
    }
    Ok(Some(parsed))
}

#[cfg(not(target_arch = "wasm32"))]
type Stamp = (u64, Option<std::time::SystemTime>);

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::type_complexity)]
fn file_cache() -> &'static Mutex<HashMap<String, (Stamp, Arc<Vec<Solid>>)>> {
    static C: OnceLock<Mutex<HashMap<String, (Stamp, Arc<Vec<Solid>>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(not(target_arch = "wasm32"))]
fn file_stamp(path: &str) -> Result<Stamp, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    Ok((meta.len(), meta.modified().ok()))
}

/// Seed the file cache with solids an importer already parsed from `path`,
/// so the first evaluation of the imported document doesn't parse again.
#[cfg(not(target_arch = "wasm32"))]
pub fn prime_file(path: &str, solids: Vec<Solid>) {
    if let Ok(stamp) = file_stamp(path) {
        file_cache()
            .lock()
            .expect("step file cache poisoned")
            .insert(path.to_string(), (stamp, Arc::new(solids)));
    }
}

/// Native filesystem fallback, cached per file.
///
/// A STEP import writes one `StepImport` node per solid, all naming the same
/// file. Without this cache every node re-parsed the whole file, so an
/// N-solid assembly cost N full parses (Voron 2.4: ~2k solids × a 240 MB
/// file). Keyed by path and validated against the file's size + mtime, so an
/// edited file on disk is re-read.
#[cfg(not(target_arch = "wasm32"))]
pub fn solids_from_file(path: &str) -> Result<Arc<Vec<Solid>>, String> {
    let stamp = file_stamp(path)?;
    // Hold the lock across the parse: concurrent evaluations of the same
    // document would otherwise all parse the same file at once.
    let mut c = file_cache().lock().expect("step file cache poisoned");
    if let Some((s, solids)) = c.get(path) {
        if *s == stamp {
            return Ok(solids.clone());
        }
    }
    let parsed = Arc::new(Solid::from_step_all(path).map_err(|e| e.to_string())?);
    c.insert(path.to_string(), (stamp, parsed.clone()));
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unregistered_path_is_none() {
        assert!(solids("/no/such/registered.step").unwrap().is_none());
    }

    #[test]
    fn bad_bytes_error_rather_than_vanish() {
        register("/tmp/bogus-registry-test.step", b"not a step file".to_vec());
        assert!(solids("/tmp/bogus-registry-test.step").is_err());
        unregister("/tmp/bogus-registry-test.step");
        assert!(!is_registered("/tmp/bogus-registry-test.step"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_parse_is_cached_and_invalidated_by_edits() {
        let dir = std::env::temp_dir().join(format!("vcad-step-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("one.step");
        let path = p.to_str().unwrap();
        let mut box_step = Solid::cube(10.0, 10.0, 10.0)
            .to_step_buffer()
            .expect("write step");
        std::fs::write(&p, &box_step).unwrap();
        let a = solids_from_file(path).unwrap();
        let b = solids_from_file(path).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "second lookup must reuse the parse");
        // A different file length invalidates the entry.
        box_step.extend_from_slice(b"\n");
        std::fs::write(&p, &box_step).unwrap();
        let c = solids_from_file(path).unwrap();
        assert!(!Arc::ptr_eq(&a, &c), "an edited file is re-parsed");
        assert!(solids_from_file(dir.join("missing.step").to_str().unwrap()).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
