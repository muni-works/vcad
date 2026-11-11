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
}

/// One cached filesystem parse: the file's identity when read, and its
/// solids. A slot has its own lock so concurrent evaluations of a document's
/// many `StepImport` nodes wait for one parse instead of each running their
/// own.
#[cfg(not(target_arch = "wasm32"))]
type FileSlot = Arc<Mutex<Option<(std::time::SystemTime, u64, Arc<Vec<Solid>>)>>>;

#[cfg(not(target_arch = "wasm32"))]
fn file_cache() -> &'static Mutex<Vec<(String, FileSlot)>> {
    static CACHE: OnceLock<Mutex<Vec<(String, FileSlot)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Files whose parse stays cached (most recent first). A document of an
/// assembly holds one file; a few covers switching between documents.
#[cfg(not(target_arch = "wasm32"))]
const FILE_CACHE_LEN: usize = 4;

/// The solids of the STEP file at `path`, parsed once per file version.
///
/// Every `StepImport` node names the whole file plus a solid index, so
/// without this a document with one root per assembly occurrence re-read
/// and re-parsed the file once per root: 550 parses of a 30 MB SolidWorks
/// assembly. Keyed on path, modification time and length, so an edited file
/// is read again.
#[cfg(not(target_arch = "wasm32"))]
pub fn solids_from_file(path: &str) -> Result<Arc<Vec<Solid>>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let stamp = (meta.modified().unwrap_or(std::time::UNIX_EPOCH), meta.len());
    let slot: FileSlot = {
        let mut cache = file_cache().lock().expect("step file cache poisoned");
        let slot = match cache.iter().position(|(p, _)| p == path) {
            Some(i) => cache.remove(i).1,
            None => Arc::new(Mutex::new(None)),
        };
        cache.insert(0, (path.to_owned(), slot.clone()));
        cache.truncate(FILE_CACHE_LEN);
        slot
    };
    let mut entry = slot.lock().expect("step file slot poisoned");
    if let Some((t, len, solids)) = entry.as_ref() {
        if (*t, *len) == stamp {
            return Ok(solids.clone());
        }
    }
    let solids = Arc::new(Solid::from_step_all(path).map_err(|e| e.to_string())?);
    *entry = Some((stamp.0, stamp.1, solids.clone()));
    Ok(solids)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod file_cache_tests {
    use super::*;

    /// Every `StepImport` node of a document names the same file; the
    /// second lookup must reuse the first parse, and an edited file must
    /// be read again.
    #[test]
    fn file_parse_is_reused_until_the_file_changes() {
        let dir = std::env::temp_dir().join(format!("vcad-step-fcache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.step");
        Solid::cube(1.0, 2.0, 3.0).to_step(&path).unwrap();
        let p = path.to_str().unwrap();
        let a = solids_from_file(p).unwrap();
        let b = solids_from_file(p).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        // Different length → different stamp, even within one mtime tick.
        Solid::cube(10.0, 20.0, 30.5).to_step(&path).unwrap();
        let c = solids_from_file(p).unwrap();
        assert!(!Arc::ptr_eq(&a, &c));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
