//! vcad.app hosts the Rust MCP core (`vcad-mcp`): one dispatcher, one tool
//! implementation, the same code `vcad mcp` runs headless.
//!
//! The app hands over one C callback. The core calls it for everything only
//! the app can do — read and replace the live document as a named undo step,
//! apply an engine tool on the window's own engine, ⌘Z, open a file in the
//! window, serve the selection / view resources, and run host tools (camera,
//! pose, snapshot, elicitation answered in the viewport, the `@vcad/mcp` link
//! handshake). Requests and replies are JSON:
//!
//! | request `op` | reply |
//! |---|---|
//! | `front` | `{id, document}` or `{}` |
//! | `open` `{document, name?}` | `{id}` |
//! | `read` `{id}` | `{document}` |
//! | `apply` `{id, document, label, author}` | `{}` |
//! | `release` `{id}` | `{}` |
//! | `documents` | `{documents:[{id, name?}]}` |
//! | `undo` `{id}` | `{label}` |
//! | `open_path` `{path}` | `{id}` or `{}` (not handled) |
//! | `live_resources` | `{resources:[…]}` |
//! | `read_resource` `{uri}` | `{result}` or `{}` (unknown) |
//! | `engine_tool` `{id, tool, args, label}` | `{envelope}` or `{}` (run it yourself) |
//! | `host_call` `{name, args, input_responses}` | `{result: CallToolResult}` |
//! | `instance_meta` | the `io.vcad/instance` object |
//!
//! Any reply may be `{error, kind?}` (`kind`: `not_found` | `unavailable`).
//! The callback runs on core worker threads, never the caller's; it returns a
//! `malloc`ed UTF-8 string (e.g. `strdup`) the core frees, or null.

use std::ffi::{c_char, c_void, CStr, CString};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vcad_ir::Document;
use vcad_mcp::{
    AppBridge, AppStore, CommitMeta, Connection, DocumentId, DocumentSummary, Server, ServerConfig,
    StoreError, StoreEvent, StoreListener,
};

/// The app's callback: `request` JSON in, `malloc`ed reply JSON out.
pub type VcadMcpHostFn = extern "C" fn(ctx: *mut c_void, request: *const c_char) -> *mut c_char;

extern "C" {
    fn free(p: *mut c_void);
}

struct FfiBridge {
    host: VcadMcpHostFn,
    ctx: usize,
    listeners: Mutex<Vec<StoreListener>>,
}

impl FfiBridge {
    fn ask(&self, req: Value) -> Value {
        let Ok(c) = CString::new(req.to_string()) else {
            return json!({"error": "request contained NUL"});
        };
        let raw = (self.host)(self.ctx as *mut c_void, c.as_ptr());
        if raw.is_null() {
            return json!({"error": "vcad.app returned nothing", "kind": "unavailable"});
        }
        // SAFETY: the host returns a NUL-terminated malloc'ed string.
        let text = unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned();
        unsafe { free(raw as *mut c_void) };
        serde_json::from_str(&text).unwrap_or_else(|e| json!({"error": format!("bad reply: {e}")}))
    }

    fn checked(&self, req: Value, id: Option<&DocumentId>) -> Result<Value, StoreError> {
        let r = self.ask(req);
        match r.get("error").and_then(Value::as_str) {
            None => Ok(r),
            Some(e) => Err(match (r.get("kind").and_then(Value::as_str), id) {
                (Some("not_found"), Some(id)) => StoreError::NotFound(id.clone()),
                (Some("unavailable"), _) => StoreError::Unavailable(e.to_string()),
                _ => StoreError::Io(e.to_string()),
            }),
        }
    }

    fn emit(&self, e: StoreEvent) {
        let ls: Vec<StoreListener> = self
            .listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for l in ls {
            l(&e);
        }
    }
}

fn doc_of(v: &Value) -> Result<Document, StoreError> {
    // An empty window (no document yet, or a bare `{nodes:{}, roots:[]}`) is
    // the empty document — never "no window".
    let empty = |k: &str| {
        v.get(k).is_none_or(|x| {
            x.as_object().is_some_and(|o| o.is_empty())
                || x.as_array().is_some_and(|a| a.is_empty())
        })
    };
    if v.is_null() || (empty("nodes") && empty("roots")) {
        return Ok(Document::default());
    }
    vcad_mcp::store::unwrap_document(v.clone())
        .ok_or_else(|| StoreError::Io("vcad.app returned a non-document".into()))
}

fn opt(v: &Value) -> Option<Result<Value, StoreError>> {
    if v.get("error").is_some() {
        return Some(Err(StoreError::Io(
            v["error"].as_str().unwrap_or("error").to_string(),
        )));
    }
    v.get("result")
        .or_else(|| v.get("envelope"))
        .cloned()
        .map(Ok)
}

impl AppBridge for FfiBridge {
    fn front_document(&self) -> Option<(DocumentId, Document)> {
        let r = self.ask(json!({"op": "front"}));
        let id = r["id"].as_str()?;
        Some((DocumentId::from(id), doc_of(&r["document"]).ok()?))
    }
    fn open(&self, doc: Document, name: Option<&str>) -> Result<DocumentId, StoreError> {
        let r = self.checked(json!({"op": "open", "document": doc, "name": name}), None)?;
        r["id"]
            .as_str()
            .map(DocumentId::from)
            .ok_or_else(|| StoreError::Io("open: no id".into()))
    }
    fn read(&self, id: &DocumentId) -> Result<Document, StoreError> {
        let r = self.checked(json!({"op": "read", "id": id.0}), Some(id))?;
        doc_of(&r["document"])
    }
    fn apply(&self, id: &DocumentId, doc: Document, meta: &CommitMeta) -> Result<(), StoreError> {
        self.checked(
            json!({"op": "apply", "id": id.0, "document": doc, "label": meta.label, "author": meta.author}),
            Some(id),
        )?;
        self.emit(StoreEvent::Changed(id.clone()));
        Ok(())
    }
    fn release(&self, id: &DocumentId) -> Result<(), StoreError> {
        self.checked(json!({"op": "release", "id": id.0}), Some(id))
            .map(|_| ())
    }
    fn documents(&self) -> Vec<DocumentSummary> {
        self.ask(json!({"op": "documents"}))["documents"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|d| {
                        Some(DocumentSummary {
                            id: DocumentId::from(d["id"].as_str()?),
                            name: d["name"].as_str().map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    fn watch(&self, listener: StoreListener) {
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(listener);
    }
    fn undo(&self, id: &DocumentId) -> Result<String, StoreError> {
        let r = self.ask(json!({"op": "undo", "id": id.0}));
        if let Some(e) = r["error"].as_str() {
            return Err(StoreError::Unavailable(e.to_string()));
        }
        self.emit(StoreEvent::Changed(id.clone()));
        Ok(r["label"].as_str().unwrap_or("").to_string())
    }
    fn open_path(&self, path: &std::path::Path) -> Result<Option<DocumentId>, StoreError> {
        let r = self.checked(
            json!({"op": "open_path", "path": path.display().to_string()}),
            None,
        )?;
        Ok(r["id"].as_str().map(DocumentId::from))
    }
    fn live_resources(&self) -> Vec<Value> {
        self.ask(json!({"op": "live_resources"}))["resources"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
    fn read_live_resource(&self, uri: &str) -> Option<Result<Value, StoreError>> {
        opt(&self.ask(json!({"op": "read_resource", "uri": uri})))
    }
    fn engine_tool(
        &self,
        id: &DocumentId,
        tool: &str,
        args: &Value,
        label: &str,
    ) -> Option<Result<Value, StoreError>> {
        let r = opt(&self.ask(
            json!({"op": "engine_tool", "id": id.0, "tool": tool, "args": args, "label": label}),
        ));
        if matches!(&r, Some(Ok(env)) if env["ok"] == json!(true)) {
            self.emit(StoreEvent::Changed(id.clone()));
        }
        r
    }
    fn host_call(
        &self,
        name: &str,
        args: &Value,
        input_responses: &Value,
    ) -> Option<Result<Value, StoreError>> {
        opt(&self.ask(
            json!({"op": "host_call", "name": name, "args": args, "input_responses": input_responses}),
        ))
    }
}

/// A running app-hosted core.
pub struct VcadMcpApp {
    server: Server,
    bridge: Arc<FfiBridge>,
    port: u16,
}

/// Tools that change the document carry the live viewer (MCP Apps `_meta.ui`).
fn viewer_hook(viewer_uri: String) -> Arc<dyn Fn(&mut Value) + Send + Sync> {
    let engine_reads = vcad_mcp::tools::engine::READ_TOOLS;
    let engine: Vec<String> = vcad_mcp::tools::engine::tools()
        .into_iter()
        .map(|t| t.name)
        .filter(|n| !engine_reads.contains(&n.as_str()))
        .collect();
    let extra = [
        "set_parameters",
        "create_cad_loon",
        "adopt_document",
        "undo",
    ];
    Arc::new(move |t: &mut Value| {
        let name = t["name"].as_str().unwrap_or("").to_string();
        if let Some(m) = vcad_mcp::tools::host::listing_meta(&name) {
            t["_meta"] = m;
        } else if engine.contains(&name) || extra.contains(&name.as_str()) {
            t["_meta"] = json!({"ui": {"resourceUri": viewer_uri}});
        }
    })
}

/// Agent jobs → the app's header progress pill: every `BusEvent::Job` becomes
/// one `{"op":"job","job":{…}}` host call, in order, on a dedicated thread (a
/// job event can fire on any worker, including one blocked on the main actor).
fn forward_jobs(server: &Server, bridge: &Arc<FfiBridge>) {
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    let tx = Mutex::new(tx);
    server
        .bus()
        .on(Arc::new(move |evt: &vcad_mcp::tasks::BusEvent| {
            if let vcad_mcp::tasks::BusEvent::Job(j) = evt {
                let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(j.clone());
            }
        }));
    let weak = Arc::downgrade(bridge);
    std::thread::Builder::new()
        .name("vcad-mcp-jobs".into())
        .spawn(move || {
            for job in rx {
                let Some(b) = weak.upgrade() else { break };
                b.ask(json!({"op": "job", "job": job}));
            }
        })
        .ok();
}

/// Start the core over the app's callback. `config_json`:
/// `{name?, version?, instructions?, port? (first to try, default 47361),
///   attempts? (default 8), serve_http? (default true), proxy? (default true),
///   node_cmd?, viewer_uri?}`.
/// Returns null (see `vcad_last_error`) when no port could be bound.
#[no_mangle]
pub extern "C" fn vcad_mcp_app_start(
    host: VcadMcpHostFn,
    ctx: *mut c_void,
    config_json: *const c_char,
) -> *mut VcadMcpApp {
    crate::err::clear_error();
    let cfg: Value = if config_json.is_null() {
        json!({})
    } else {
        serde_json::from_str(&unsafe { CStr::from_ptr(config_json) }.to_string_lossy())
            .unwrap_or_else(|_| json!({}))
    };
    let bridge = Arc::new(FfiBridge {
        host,
        ctx: ctx as usize,
        listeners: Mutex::new(Vec::new()),
    });
    let mut config = ServerConfig::default();
    if let Some(n) = cfg["name"].as_str() {
        config.name = n.into();
    }
    if let Some(v) = cfg["version"].as_str() {
        config.version = v.into();
    }
    if let Some(i) = cfg["instructions"].as_str() {
        config.instructions = Some(i.into());
    }
    config.proxy = if cfg["proxy"].as_bool() == Some(false) {
        None
    } else {
        match cfg["node_cmd"].as_str() {
            Some(cmd) => {
                let mut parts = cmd.split_whitespace().map(str::to_string);
                parts.next().map(|program| vcad_mcp::proxy::ProxyConfig {
                    program,
                    args: parts.collect(),
                    env: Vec::new(),
                })
            }
            None => vcad_mcp::proxy::ProxyConfig::from_env(),
        }
    };
    let b2 = Arc::downgrade(&bridge);
    config.instance_meta = Some(Arc::new(move || {
        b2.upgrade()
            .map(|b| b.ask(json!({"op": "instance_meta"})))
            .filter(|v| v.get("error").is_none())
            .unwrap_or_else(|| json!({}))
    }));
    config.listing_hook = Some(viewer_hook(
        cfg["viewer_uri"]
            .as_str()
            .unwrap_or("ui://vcad/viewer")
            .to_string(),
    ));
    let server = Server::new(Arc::new(AppStore::with_bridge(bridge.clone())), config);
    forward_jobs(&server, &bridge);
    let mut port = 0u16;
    if cfg["serve_http"].as_bool() != Some(false) {
        let first = cfg["port"].as_u64().unwrap_or(47_361) as u16;
        let attempts = cfg["attempts"].as_u64().unwrap_or(8) as u16;
        let mut listener = None;
        for p in first..=first.saturating_add(attempts) {
            if let Ok(l) =
                vcad_mcp::transport::bind_http(SocketAddr::from((Ipv4Addr::LOCALHOST, p)))
            {
                port = l.local_addr().map(|a| a.port()).unwrap_or(p);
                listener = Some(l);
                break;
            }
        }
        let Some(l) = listener else {
            crate::err::set_error(format!(
                "vcad_mcp_app_start: no free loopback port in {first}..={}",
                first.saturating_add(attempts)
            ));
            return std::ptr::null_mut();
        };
        let s2 = server.clone();
        std::thread::Builder::new()
            .name("vcad-mcp-http".into())
            .spawn(move || vcad_mcp::transport::serve_http(&s2, l))
            .ok();
    }
    Box::into_raw(Box::new(VcadMcpApp {
        server,
        bridge,
        port,
    }))
}

/// The bound loopback port (0 when not serving HTTP).
#[no_mangle]
pub extern "C" fn vcad_mcp_app_port(app: *const VcadMcpApp) -> u16 {
    if app.is_null() {
        return 0;
    }
    unsafe { &*app }.port
}

/// One JSON-RPC message through the same dispatcher the HTTP endpoint uses
/// (lenient, no handshake needed). Owned reply JSON — free with
/// `vcad_string_free` — or null for a notification. Must not be called on the
/// thread the host callback needs (the app's main thread).
#[no_mangle]
pub extern "C" fn vcad_mcp_app_call(
    app: *const VcadMcpApp,
    msg_json: *const c_char,
) -> *mut c_char {
    if app.is_null() || msg_json.is_null() {
        return std::ptr::null_mut();
    }
    let app = unsafe { &*app };
    let msg: Value =
        match serde_json::from_str(&unsafe { CStr::from_ptr(msg_json) }.to_string_lossy()) {
            Ok(v) => v,
            Err(e) => {
                crate::err::set_error(format!("vcad_mcp_app_call: {e}"));
                return std::ptr::null_mut();
            }
        };
    let conn = Connection::one_shot(app.server.clone(), Arc::new(|_| {}));
    match conn.handle(msg, None).body {
        Some(b) => CString::new(b.to_string()).map_or(std::ptr::null_mut(), CString::into_raw),
        None => std::ptr::null_mut(),
    }
}

/// A live resource changed (`vcad://doc/selection`, `vcad://doc/view`, …):
/// notify subscribed clients.
#[no_mangle]
pub extern "C" fn vcad_mcp_app_resource_updated(app: *const VcadMcpApp, uri: *const c_char) {
    if app.is_null() || uri.is_null() {
        return;
    }
    let uri = unsafe { CStr::from_ptr(uri) }
        .to_string_lossy()
        .into_owned();
    unsafe { &*app }
        .server
        .bus()
        .emit(vcad_mcp::tasks::BusEvent::ResourceUpdated(uri));
}

/// The user changed document `id` (or, with null, the front window changed):
/// `vcad://doc/{id}` / `vcad://doc/current` subscribers hear about it.
#[no_mangle]
pub extern "C" fn vcad_mcp_app_document_changed(app: *const VcadMcpApp, id: *const c_char) {
    if app.is_null() {
        return;
    }
    let app = unsafe { &*app };
    if id.is_null() {
        app.bridge.emit(StoreEvent::CurrentChanged(None));
    } else {
        let id = unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned();
        app.bridge
            .emit(StoreEvent::Changed(DocumentId::from(id.as_str())));
    }
}

/// The core's host-tool definitions (`host_tools.json`), owned; free with
/// `vcad_string_free`. The app checks its own implementations against these.
#[no_mangle]
pub extern "C" fn vcad_mcp_host_tools() -> *mut c_char {
    CString::new(vcad_mcp::tools::host::HOST_TOOLS_JSON)
        .map_or(std::ptr::null_mut(), CString::into_raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A minimal in-process "app": one window, history, host echo.
    #[derive(Default)]
    struct App {
        doc: Mutex<(Value, Vec<(Value, String)>)>,
        ops: Mutex<Vec<String>>,
    }

    extern "C" fn host(ctx: *mut c_void, req: *const c_char) -> *mut c_char {
        let app = unsafe { &*(ctx as *const App) };
        let req: Value =
            serde_json::from_str(&unsafe { CStr::from_ptr(req) }.to_string_lossy()).unwrap();
        let op = req["op"].as_str().unwrap().to_string();
        app.ops.lock().unwrap().push(op.clone());
        let mut d = app.doc.lock().unwrap();
        let reply = match op.as_str() {
            "front" => json!({"id": "doc_1_AAAAxyz", "document": d.0}),
            "read" => json!({"document": d.0}),
            "apply" => {
                let prev = std::mem::replace(&mut d.0, req["document"].clone());
                d.1.push((prev, req["label"].as_str().unwrap().to_string()));
                json!({})
            }
            "undo" => match d.1.pop() {
                Some((prev, label)) => {
                    d.0 = prev;
                    json!({"label": label})
                }
                None => json!({"error": "Nothing to undo"}),
            },
            "documents" => json!({"documents": [{"id": "doc_1_AAAAxyz", "name": "Untitled"}]}),
            "host_call" => {
                json!({"result": {"content": [{"type": "text", "text": format!("host:{}", req["name"].as_str().unwrap())}]}})
            }
            "instance_meta" => json!({"pid": 7, "document": "Untitled"}),
            _ => json!({}),
        };
        let c = CString::new(reply.to_string()).unwrap();
        // Hand back a malloc'ed copy, as the Swift side does with strdup.
        extern "C" {
            fn strdup(s: *const c_char) -> *mut c_char;
        }
        unsafe { strdup(c.as_ptr()) }
    }

    fn call(app: *const VcadMcpApp, name: &str, args: Value) -> Value {
        let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}});
        let c = CString::new(msg.to_string()).unwrap();
        let raw = vcad_mcp_app_call(app, c.as_ptr());
        assert!(!raw.is_null());
        let s = unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned();
        crate::json::vcad_string_free(raw);
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn app_store_round_trip_over_the_c_abi() {
        let state = Box::leak(Box::new(App::default()));
        state.doc.lock().unwrap().0 = serde_json::to_value(Document::default()).unwrap();
        let cfg = CString::new(r#"{"serve_http": false, "proxy": false}"#).unwrap();
        let app = vcad_mcp_app_start(host, state as *const App as *mut c_void, cfg.as_ptr());
        assert!(!app.is_null());
        assert_eq!(vcad_mcp_app_port(app), 0);

        let r = call(
            app,
            "create",
            json!({"type": "cube", "name": "Block", "params": {"size": {"x": 1, "y": 2, "z": 3}}}),
        );
        assert_ne!(r["result"]["isError"], json!(true), "{r}");
        assert_eq!(r["result"]["_meta"]["io.vcad/instance"]["pid"], 7);
        let labels: Vec<String> = state
            .doc
            .lock()
            .unwrap()
            .1
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        assert_eq!(labels, vec!["Create Block"]);
        assert!(!state.doc.lock().unwrap().0["roots"]
            .as_array()
            .unwrap()
            .is_empty());

        let r = call(app, "set_camera", json!({"azimuth": 10}));
        assert_eq!(r["result"]["content"][0]["text"], "host:set_camera");

        let r = call(app, "undo", json!({}));
        assert_eq!(r["result"]["structuredContent"]["undone"], "Create Block");
        assert!(state.doc.lock().unwrap().0["roots"]
            .as_array()
            .unwrap()
            .is_empty());

        // The engine tools asked the app first (engine_tool), then fell back.
        let ops = state.ops.lock().unwrap().clone();
        assert!(ops.contains(&"engine_tool".to_string()), "{ops:?}");

        // Listing carries the viewer on mutating tools only.
        let msg = CString::new(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
        let raw = vcad_mcp_app_call(app, msg.as_ptr());
        let list: Value =
            serde_json::from_str(&unsafe { CStr::from_ptr(raw) }.to_string_lossy()).unwrap();
        crate::json::vcad_string_free(raw);
        let by: BTreeMap<String, Value> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["name"].as_str().unwrap().to_string(), t.clone()))
            .collect();
        assert!(by["create"]["_meta"]["ui"]["resourceUri"].is_string());
        assert!(by["fillet_edges"]["_meta"]["ui"]["resourceUri"].is_string());
        assert!(by["describe_scene"].get("_meta").is_none());
    }
}
