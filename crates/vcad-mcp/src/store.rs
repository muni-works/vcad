//! The `DocumentStore` seam: where an MCP session's document lives.
//!
//! Two implementations share one contract:
//! - [`HeadlessStore`]: one JSON file per session under `~/.vcad/mcp-sessions`
//!   (override `VCAD_MCP_SESSION_DIR`). Byte-compatible with the node server's
//!   `FileSessionStore` (`packages/mcp/src/session-store.ts`): the file is
//!   `<document_id>.json` holding the raw Document IR, written
//!   write-then-rename. Either server can open the other's sessions.
//! - [`AppStore`]: the live document inside vcad.app (M1 wires it through
//!   `vcad-ffi`). M0 ships the type so the server can be generic over it.
//!
//! **API stability:** M1–M4 build on this trait. Add methods with default
//! bodies only; never change an existing signature.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use vcad_ir::Document;

/// An MCP session handle (`document_id`), e.g. `doc_3_Ab12xyz…`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentId(pub String);

impl DocumentId {
    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DocumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for DocumentId {
    fn from(s: &str) -> Self {
        DocumentId(s.to_string())
    }
}

/// Which backend a store is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    /// Files on disk; no UI attached.
    Headless,
    /// The live document in a running vcad.app.
    App,
}

impl StoreKind {
    /// Wire name (`server_info.store`).
    pub fn as_str(self) -> &'static str {
        match self {
            StoreKind::Headless => "headless",
            StoreKind::App => "app",
        }
    }
}

/// Who made an edit, and what to call it. The App store turns one commit into
/// one named undo step ("each agent turn = one named undo step", M1).
#[derive(Debug, Clone, Default)]
pub struct CommitMeta {
    /// Human-readable undo label, usually the tool name ("create_cad_loon").
    pub label: String,
    /// `"agent"` for MCP tool calls; a user id or `"human"` otherwise.
    pub author: String,
}

impl CommitMeta {
    /// An agent edit labelled `label`.
    pub fn agent(label: impl Into<String>) -> Self {
        CommitMeta {
            label: label.into(),
            author: "agent".into(),
        }
    }
}

/// One row of [`DocumentStore::list`].
#[derive(Debug, Clone)]
pub struct DocumentSummary {
    /// Session id.
    pub id: DocumentId,
    /// Display name, when one is known.
    pub name: Option<String>,
}

/// A change notification (feeds `resources/updated` / `subscriptions/listen`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreEvent {
    /// A document's content changed (by any author).
    Changed(DocumentId),
    /// A document was closed / dropped.
    Closed(DocumentId),
    /// The store's "current" document changed (App: the front window).
    CurrentChanged(Option<DocumentId>),
}

/// Errors a store can report. `Lost` vs `NotFound` keeps the node server's
/// distinction between "the server restarted" and "typo".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// No such document and nothing suggests it ever existed here.
    NotFound(DocumentId),
    /// The id was minted by a process that is gone and nothing durable held it.
    Lost(DocumentId),
    /// The id is not a well-formed session id (path-escape guard).
    InvalidId(String),
    /// I/O or (de)serialization failure.
    Io(String),
    /// The backend is not available (e.g. vcad.app not running / M1 pending).
    Unavailable(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NotFound(id) => write!(f, "Unknown document_id '{id}'"),
            StoreError::Lost(id) => write!(
                f,
                "SESSION LOST TO A SERVER RESTART: document_id '{id}' was minted by a previous server process and was not persisted. Re-run the last authoring call."
            ),
            StoreError::InvalidId(id) => write!(f, "Invalid document_id '{id}'"),
            StoreError::Io(e) => write!(f, "document store I/O error: {e}"),
            StoreError::Unavailable(e) => write!(f, "document store unavailable: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Callback receiving store events. Must not block.
pub type StoreListener = Arc<dyn Fn(&StoreEvent) + Send + Sync>;

/// Where MCP documents live. Implementations must be cheap to call from any
/// thread; the server holds one `Arc<dyn DocumentStore>`.
pub trait DocumentStore: Send + Sync {
    /// Which backend this is.
    fn kind(&self) -> StoreKind;
    /// True when documents survive a server restart.
    fn durable(&self) -> bool;
    /// Register a new document; returns its freshly minted id.
    fn create(&self, doc: Document, name: Option<&str>) -> Result<DocumentId, StoreError>;
    /// Read a document. Returns an owned snapshot.
    fn load(&self, id: &DocumentId) -> Result<Document, StoreError>;
    /// Replace a document's content. One call = one undo step on the App store.
    fn commit(&self, id: &DocumentId, doc: Document, meta: CommitMeta) -> Result<(), StoreError>;
    /// Forget a document (`close_document`). Idempotent.
    fn close(&self, id: &DocumentId) -> Result<(), StoreError>;
    /// Documents this store knows about.
    fn list(&self) -> Result<Vec<DocumentSummary>, StoreError>;
    /// The "current" document (App: the front window; Headless: the most
    /// recently created/committed in this process). Backs `vcad://doc/current`.
    fn current(&self) -> Option<DocumentId> {
        None
    }
    /// Subscribe to change events. Default: events are never emitted.
    fn watch(&self, _listener: StoreListener) {}
    /// Where documents live on disk, if anywhere (`server_info.session_dir`).
    fn location(&self) -> Option<PathBuf> {
        None
    }
    /// Undo the last commit to `id`; returns the undone step's label.
    /// App: exactly ⌘Z. Headless: the native commit history of this process.
    fn undo(&self, _id: &DocumentId) -> Result<String, StoreError> {
        Err(StoreError::Unavailable(
            "this store keeps no undo history".into(),
        ))
    }
    /// Open a file (`.vcad` / `.loon`) the store's own way. `Ok(None)` means
    /// "not handled here": the caller loads the file and calls [`Self::create`].
    /// App: the file opens in the window (loon files keep hot-reloading).
    fn open_path(&self, _path: &std::path::Path) -> Result<Option<DocumentId>, StoreError> {
        Ok(None)
    }
    /// Extra live resources beyond `vcad://doc/*` documents (`resources/list`
    /// entries). App: `vcad://doc/selection`, `vcad://doc/view`, the viewer.
    fn live_resources(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }
    /// Read a live resource (`resources/read` result), `None` when unknown.
    fn read_live_resource(&self, _uri: &str) -> Option<Result<serde_json::Value, StoreError>> {
        None
    }
    /// Apply one of the engine's agent tools (`vcad_app` lowering) on the
    /// store's own live engine, when it keeps one: vcad.app does, so feature
    /// history, the agent cursor and named undo stay exactly the app's.
    /// Returns the engine envelope (`{"ok":true,"result":…}` /
    /// `{"ok":false,"error":…}`). `None`: the caller runs the tool itself.
    fn engine_tool(
        &self,
        _id: &DocumentId,
        _tool: &str,
        _args: &serde_json::Value,
        _label: &str,
    ) -> Option<Result<serde_json::Value, StoreError>> {
        None
    }
    /// Run a host tool — one that needs the app's viewport, panels or user
    /// (`tools/host_tools.json`). `input_responses` is the MRTR
    /// `inputResponses` the client already gave. `None`: no host here
    /// (headless); the caller reports that vcad.app is needed.
    fn host_call(
        &self,
        _name: &str,
        _args: &serde_json::Value,
        _input_responses: &serde_json::Value,
    ) -> Option<Result<serde_json::Value, StoreError>> {
        None
    }
}

// ─── Session ids (compatible with the node server) ───────────────────────────

/// Length of the boot-token prefix carried in an id's suffix segment.
const BOOT_TOKEN_LEN: usize = 4;

fn random_b64url(n_bytes: usize) -> String {
    use base64::Engine;
    let mut buf = vec![0u8; n_bytes];
    if getrandom::getrandom(&mut buf).is_err() {
        // Extremely unlikely; fall back to time-derived bytes.
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (t >> ((i % 16) * 8)) as u8 ^ (i as u8).wrapping_mul(31);
        }
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf)
}

/// This process's boot token: fresh per process, stable for its life.
pub fn boot_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let mut t = random_b64url(6);
        t.truncate(BOOT_TOKEN_LEN);
        t
    })
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Mint a session id: `doc_<n>_<boot><random>` (same shape as node).
pub fn mint_document_id() -> DocumentId {
    let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    DocumentId(format!("doc_{n}_{}{}", boot_token(), random_b64url(9)))
}

/// The boot token embedded in an id, if it carries one.
pub fn id_boot_token(id: &str) -> Option<&str> {
    let rest = id.strip_prefix("doc_")?;
    let (n, suffix) = rest.split_once('_')?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) || suffix.len() < BOOT_TOKEN_LEN {
        return None;
    }
    if !suffix.bytes().all(valid_id_byte) {
        return None;
    }
    Some(&suffix[..BOOT_TOKEN_LEN])
}

/// True when `id` was minted by a different process than this one.
pub fn is_foreign_id(id: &str) -> bool {
    id_boot_token(id).is_some_and(|t| t != boot_token())
}

fn valid_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Same guard as node's `pathFor`: `^[A-Za-z0-9_-]{1,128}$`.
pub fn is_valid_id(id: &str) -> bool {
    // `current` / `selection` / `view` name app resources under vcad://doc/.
    !matches!(id, "current" | "selection" | "view")
        && !id.is_empty()
        && id.len() <= 128
        && id.bytes().all(valid_id_byte)
}

/// Unwrap a stored blob into a Document; tolerate the legacy `{ ir }` envelope.
pub fn unwrap_document(value: serde_json::Value) -> Option<Document> {
    let looks = |v: &serde_json::Value| v.get("nodes").is_some() && v.get("roots").is_some();
    if looks(&value) {
        return serde_json::from_value(value).ok();
    }
    let ir = value.get("ir")?.clone();
    if looks(&ir) {
        return serde_json::from_value(ir).ok();
    }
    None
}

// ─── HeadlessStore ───────────────────────────────────────────────────────────

/// Session directory: `VCAD_MCP_SESSION_DIR`, else `~/.vcad/mcp-sessions`.
pub fn default_session_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("VCAD_MCP_SESSION_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    vcad_home().join("mcp-sessions")
}

/// `~/.vcad` (or `$VCAD_HOME`).
pub fn vcad_home() -> PathBuf {
    if let Some(d) = std::env::var_os("VCAD_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".vcad")
}

/// False when `VCAD_MCP_DISK_SESSIONS` is `0|false|off` (node's opt-out).
pub fn disk_sessions_enabled() -> bool {
    !matches!(
        std::env::var("VCAD_MCP_DISK_SESSIONS")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "off"
    )
}

/// File-per-session store, compatible with node's `FileSessionStore`.
///
/// With `dir = None` it is purely in-memory (the `VCAD_MCP_DISK_SESSIONS=0`
/// mode). Disk reads are not cached: the node proxy writes the same files, so
/// every `load` reads the current bytes.
pub struct HeadlessStore {
    dir: Option<PathBuf>,
    mem: Mutex<std::collections::BTreeMap<DocumentId, (Document, Option<String>)>>,
    current: Mutex<Option<DocumentId>>,
    listeners: Mutex<Vec<StoreListener>>,
    history: Mutex<std::collections::BTreeMap<DocumentId, Vec<(Document, String)>>>,
}

/// Undo steps kept per headless document.
const HEADLESS_UNDO_DEPTH: usize = 64;

impl HeadlessStore {
    /// Disk-backed store rooted at `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        HeadlessStore {
            dir: Some(dir.into()),
            mem: Mutex::new(Default::default()),
            current: Mutex::new(None),
            listeners: Mutex::new(Vec::new()),
            history: Mutex::new(Default::default()),
        }
    }

    /// Non-durable store (process memory only).
    pub fn in_memory() -> Self {
        HeadlessStore {
            dir: None,
            mem: Mutex::new(Default::default()),
            current: Mutex::new(None),
            listeners: Mutex::new(Vec::new()),
            history: Mutex::new(Default::default()),
        }
    }

    /// Store per environment: disk at [`default_session_dir`] unless opted out.
    pub fn from_env() -> Self {
        if disk_sessions_enabled() {
            Self::new(default_session_dir())
        } else {
            Self::in_memory()
        }
    }

    /// Write `doc` under an existing id (no history, no NotFound check) —
    /// the core mirroring an app document for the node upstream.
    pub fn put(&self, id: &DocumentId, doc: Document) -> Result<(), StoreError> {
        if let Some(path) = self.path_for(id)? {
            Self::write_file(&path, &doc)?;
        }
        self.mem
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), (doc, None));
        Ok(())
    }

    fn write_doc(&self, id: &DocumentId, doc: Document) -> Result<(), StoreError> {
        match self.path_for(id)? {
            Some(path) => {
                if !path.exists()
                    && !self
                        .mem
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .contains_key(id)
                {
                    return Err(StoreError::NotFound(id.clone()));
                }
                Self::write_file(&path, &doc)?;
                let mut mem = self.mem.lock().unwrap_or_else(|e| e.into_inner());
                let name = mem.get(id).and_then(|(_, n)| n.clone());
                mem.insert(id.clone(), (doc, name));
            }
            None => {
                let mut mem = self.mem.lock().unwrap_or_else(|e| e.into_inner());
                let entry = mem
                    .get_mut(id)
                    .ok_or_else(|| StoreError::NotFound(id.clone()))?;
                entry.0 = doc;
            }
        }
        self.set_current(id);
        self.emit(StoreEvent::Changed(id.clone()));
        Ok(())
    }

    fn close_doc(&self, id: &DocumentId) -> Result<(), StoreError> {
        if let Some(path) = self.path_for(id)? {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(StoreError::Io(e.to_string())),
            }
        }
        self.mem
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        {
            let mut cur = self.current.lock().unwrap_or_else(|e| e.into_inner());
            if cur.as_ref() == Some(id) {
                *cur = None;
            }
        }
        self.emit(StoreEvent::Closed(id.clone()));
        Ok(())
    }

    fn path_for(&self, id: &DocumentId) -> Result<Option<PathBuf>, StoreError> {
        if !is_valid_id(id.as_str()) {
            return Err(StoreError::InvalidId(id.0.clone()));
        }
        Ok(self.dir.as_ref().map(|d| d.join(format!("{}.json", id.0))))
    }

    fn emit(&self, evt: StoreEvent) {
        let listeners = self.listeners.lock().map(|l| l.clone()).unwrap_or_default();
        for l in listeners {
            l(&evt);
        }
    }

    fn set_current(&self, id: &DocumentId) {
        let changed = {
            let mut cur = self.current.lock().unwrap_or_else(|e| e.into_inner());
            let changed = cur.as_ref() != Some(id);
            *cur = Some(id.clone());
            changed
        };
        if changed {
            self.emit(StoreEvent::CurrentChanged(Some(id.clone())));
        }
    }

    fn write_file(path: &Path, doc: &Document) -> Result<(), StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StoreError::Io(e.to_string()))?;
        }
        let json = serde_json::to_string(doc).map_err(|e| StoreError::Io(e.to_string()))?;
        // Write-then-rename, like node: a crash mid-write keeps the last good snapshot.
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, json).map_err(|e| StoreError::Io(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| StoreError::Io(e.to_string()))
    }
}

impl DocumentStore for HeadlessStore {
    fn kind(&self) -> StoreKind {
        StoreKind::Headless
    }

    fn durable(&self) -> bool {
        self.dir.is_some()
    }

    fn create(&self, doc: Document, name: Option<&str>) -> Result<DocumentId, StoreError> {
        let id = mint_document_id();
        if let Some(path) = self.path_for(&id)? {
            Self::write_file(&path, &doc)?;
        }
        self.mem
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), (doc, name.map(str::to_string)));
        self.set_current(&id);
        self.emit(StoreEvent::Changed(id.clone()));
        Ok(id)
    }

    fn load(&self, id: &DocumentId) -> Result<Document, StoreError> {
        if let Some(path) = self.path_for(id)? {
            match std::fs::read_to_string(&path) {
                Ok(raw) => {
                    let value: serde_json::Value =
                        serde_json::from_str(&raw).map_err(|e| StoreError::Io(e.to_string()))?;
                    return unwrap_document(value).ok_or_else(|| {
                        StoreError::Io(format!("{} is not a vcad Document", path.display()))
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(StoreError::Io(e.to_string())),
            }
        } else if let Some((doc, _)) = self.mem.lock().unwrap_or_else(|e| e.into_inner()).get(id) {
            return Ok(doc.clone());
        }
        if is_foreign_id(id.as_str()) {
            Err(StoreError::Lost(id.clone()))
        } else {
            Err(StoreError::NotFound(id.clone()))
        }
    }

    fn commit(&self, id: &DocumentId, doc: Document, meta: CommitMeta) -> Result<(), StoreError> {
        let before = self.load(id).ok();
        self.write_doc(id, doc)?;
        if let Some(prev) = before {
            let mut h = self.history.lock().unwrap_or_else(|e| e.into_inner());
            let steps = h.entry(id.clone()).or_default();
            steps.push((prev, meta.label));
            if steps.len() > HEADLESS_UNDO_DEPTH {
                steps.remove(0);
            }
        }
        Ok(())
    }

    fn undo(&self, id: &DocumentId) -> Result<String, StoreError> {
        let step = self
            .history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(id)
            .and_then(Vec::pop);
        let Some((prev, label)) = step else {
            self.load(id)?;
            return Err(StoreError::Unavailable("nothing to undo".into()));
        };
        self.write_doc(id, prev)?;
        Ok(label)
    }

    fn close(&self, id: &DocumentId) -> Result<(), StoreError> {
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        self.close_doc(id)
    }

    fn list(&self) -> Result<Vec<DocumentSummary>, StoreError> {
        let mem = self.mem.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<DocumentSummary> = mem
            .iter()
            .map(|(id, (_, name))| DocumentSummary {
                id: id.clone(),
                name: name.clone(),
            })
            .collect();
        if let Some(dir) = &self.dir {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for entry in rd.flatten() {
                    let name = entry.file_name();
                    let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
                        continue;
                    };
                    if is_valid_id(stem) && !mem.contains_key(&DocumentId::from(stem)) {
                        out.push(DocumentSummary {
                            id: stem.into(),
                            name: None,
                        });
                    }
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    fn current(&self) -> Option<DocumentId> {
        self.current.lock().ok().and_then(|c| c.clone())
    }

    fn watch(&self, listener: StoreListener) {
        if let Ok(mut l) = self.listeners.lock() {
            l.push(listener);
        }
    }

    fn location(&self) -> Option<PathBuf> {
        self.dir.clone()
    }
}

// ─── AppStore (the live vcad.app document, via vcad-ffi) ──────────────────────────────────────────

/// Backend hooks the app supplies (`vcad-ffi`'s `mcp` module, over C callbacks into Swift).
///
/// Kept separate from [`DocumentStore`] so the app can plug in without this
/// crate depending on the FFI layer.
pub trait AppBridge: Send + Sync {
    /// The front window's document, if any.
    fn front_document(&self) -> Option<(DocumentId, Document)>;
    /// Open a new window/tab on `doc`; returns its id.
    fn open(&self, doc: Document, name: Option<&str>) -> Result<DocumentId, StoreError>;
    /// Read a window's document.
    fn read(&self, id: &DocumentId) -> Result<Document, StoreError>;
    /// Replace a window's document as one named undo step.
    fn apply(&self, id: &DocumentId, doc: Document, meta: &CommitMeta) -> Result<(), StoreError>;
    /// Close a window's session (the window may stay open for the user).
    fn release(&self, id: &DocumentId) -> Result<(), StoreError>;
    /// Every open document.
    fn documents(&self) -> Vec<DocumentSummary>;
    /// Subscribe to app-side changes (user edits, window focus).
    fn watch(&self, listener: StoreListener);
    /// ⌘Z on a window; returns the undone step's label.
    fn undo(&self, _id: &DocumentId) -> Result<String, StoreError> {
        Err(StoreError::Unavailable(
            "undo not supported by this app".into(),
        ))
    }
    /// Open a file in a window (see [`DocumentStore::open_path`]).
    fn open_path(&self, _path: &std::path::Path) -> Result<Option<DocumentId>, StoreError> {
        Ok(None)
    }
    /// Live resources (selection, view, viewer) — `resources/list` entries.
    fn live_resources(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }
    /// Read a live resource.
    fn read_live_resource(&self, _uri: &str) -> Option<Result<serde_json::Value, StoreError>> {
        None
    }
    /// Apply an engine tool on the window's live engine (see
    /// [`DocumentStore::engine_tool`]).
    fn engine_tool(
        &self,
        _id: &DocumentId,
        _tool: &str,
        _args: &serde_json::Value,
        _label: &str,
    ) -> Option<Result<serde_json::Value, StoreError>> {
        None
    }
    /// Run a host tool in the app (see [`DocumentStore::host_call`]).
    fn host_call(
        &self,
        _name: &str,
        _args: &serde_json::Value,
        _input_responses: &serde_json::Value,
    ) -> Option<Result<serde_json::Value, StoreError>> {
        None
    }
}

/// The live document in vcad.app. Without a bridge (M0) every call reports
/// [`StoreError::Unavailable`]; M1 constructs it with [`AppStore::with_bridge`].
pub struct AppStore {
    bridge: Option<Arc<dyn AppBridge>>,
}

impl AppStore {
    /// A store with no app attached (M0 stub).
    pub fn unattached() -> Self {
        AppStore { bridge: None }
    }

    /// A store backed by the running app.
    pub fn with_bridge(bridge: Arc<dyn AppBridge>) -> Self {
        AppStore {
            bridge: Some(bridge),
        }
    }

    fn bridge(&self) -> Result<&Arc<dyn AppBridge>, StoreError> {
        self.bridge
            .as_ref()
            .ok_or_else(|| StoreError::Unavailable("vcad.app store not attached (M1)".into()))
    }
}

impl DocumentStore for AppStore {
    fn kind(&self) -> StoreKind {
        StoreKind::App
    }
    fn durable(&self) -> bool {
        // The app owns persistence (autosave / user files).
        self.bridge.is_some()
    }
    fn create(&self, doc: Document, name: Option<&str>) -> Result<DocumentId, StoreError> {
        self.bridge()?.open(doc, name)
    }
    fn load(&self, id: &DocumentId) -> Result<Document, StoreError> {
        self.bridge()?.read(id)
    }
    fn commit(&self, id: &DocumentId, doc: Document, meta: CommitMeta) -> Result<(), StoreError> {
        self.bridge()?.apply(id, doc, &meta)
    }
    fn close(&self, id: &DocumentId) -> Result<(), StoreError> {
        self.bridge()?.release(id)
    }
    fn list(&self) -> Result<Vec<DocumentSummary>, StoreError> {
        Ok(self.bridge()?.documents())
    }
    fn current(&self) -> Option<DocumentId> {
        self.bridge.as_ref()?.front_document().map(|(id, _)| id)
    }
    fn watch(&self, listener: StoreListener) {
        if let Some(b) = &self.bridge {
            b.watch(listener);
        }
    }
    fn undo(&self, id: &DocumentId) -> Result<String, StoreError> {
        self.bridge()?.undo(id)
    }
    fn open_path(&self, path: &std::path::Path) -> Result<Option<DocumentId>, StoreError> {
        self.bridge()?.open_path(path)
    }
    fn live_resources(&self) -> Vec<serde_json::Value> {
        self.bridge
            .as_ref()
            .map(|b| b.live_resources())
            .unwrap_or_default()
    }
    fn read_live_resource(&self, uri: &str) -> Option<Result<serde_json::Value, StoreError>> {
        self.bridge.as_ref()?.read_live_resource(uri)
    }
    fn host_call(
        &self,
        name: &str,
        args: &serde_json::Value,
        input_responses: &serde_json::Value,
    ) -> Option<Result<serde_json::Value, StoreError>> {
        self.bridge.as_ref()?.host_call(name, args, input_responses)
    }
    fn engine_tool(
        &self,
        id: &DocumentId,
        tool: &str,
        args: &serde_json::Value,
        label: &str,
    ) -> Option<Result<serde_json::Value, StoreError>> {
        self.bridge.as_ref()?.engine_tool(id, tool, args, label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_match_node_shape_and_carry_boot_token() {
        let id = mint_document_id();
        assert!(id.0.starts_with("doc_"));
        assert!(is_valid_id(id.as_str()));
        assert_eq!(id_boot_token(id.as_str()), Some(boot_token()));
        assert!(!is_foreign_id(id.as_str()));
        assert!(is_foreign_id("doc_1_ZZZZabcdef"));
        assert_eq!(id_boot_token("hand-written"), None);
    }

    #[test]
    fn rejects_path_escape() {
        assert!(!is_valid_id("../etc/passwd"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("selection") && !is_valid_id("view"));
    }
}
