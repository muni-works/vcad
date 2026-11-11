//! The dual-era MCP server.
//!
//! Modern (`2026-07-28`) messages are answered statelessly per request, as in
//! `protocol-2026.ts`; legacy messages ride a [`Connection`] that remembers
//! the `initialize` handshake. Both eras share one dispatcher, so a tool
//! behaves identically whichever era called it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::protocol::*;
use crate::proxy::{Proxy, ProxyConfig};
use crate::store::{CommitMeta, DocumentId, DocumentStore, StoreEvent, StoreKind};
use crate::tasks::{Bus, BusEvent, TaskStore};
use crate::tools::core::InfoSource;
use crate::tools::{ElicitFn, ProgressFn, ToolCtx, ToolDef, ToolError, LONG_RUNNING_TOOLS};

/// Outbound message sink (notifications, server→client requests).
pub type Sink = Arc<dyn Fn(Value) + Send + Sync>;

/// Server configuration.
#[derive(Clone)]
pub struct ServerConfig {
    /// `serverInfo.name`.
    pub name: String,
    /// `serverInfo.version`.
    pub version: String,
    /// `instructions`.
    pub instructions: Option<String>,
    /// Node upstream for unported tools; `None` = native only.
    pub proxy: Option<ProxyConfig>,
    /// Names the serving instance on every tool result (vcad.app: pid, port,
    /// open document). `None` headless.
    pub instance_meta: Option<Arc<dyn Fn() -> Value + Send + Sync>>,
    /// Rewrites each `tools/list` entry (vcad.app adds the live viewer's
    /// `_meta.ui` to tools that change the document).
    pub listing_hook: Option<Arc<dyn Fn(&mut Value) + Send + Sync>>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            name: "vcad".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            instructions: Some(
                "vcad parametric CAD. Author parts with create_cad_loon, check them with inspect_cad / render_view, export with export_cad. A document_id is a handle, not storage: on any preview-shaped complaint call server_info first."
                    .into(),
            ),
            proxy: ProxyConfig::from_env(),
            instance_meta: None,
            listing_hook: None,
        }
    }
}

/// A JSON-RPC error.
#[derive(Debug, Clone)]
pub struct RpcErr {
    /// Code.
    pub code: i64,
    /// Message.
    pub message: String,
    /// Data.
    pub data: Option<Value>,
}

impl RpcErr {
    fn new(code: i64, message: impl Into<String>) -> Self {
        RpcErr {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// The reply to one message.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// HTTP status to use.
    pub status: u16,
    /// Response body; `None` = no body (notification, or a listen stream).
    pub body: Option<Value>,
    /// The request opened a `subscriptions/listen` stream (kept open by the
    /// transport until it calls [`ListenHandle::close`]).
    pub listen: Option<ListenHandle>,
    /// HTTP: a legacy `initialize` — echo this as `Mcp-Session-Id`.
    pub session_id: Option<String>,
}

impl Outcome {
    fn body(status: u16, body: Value) -> Self {
        Outcome {
            status,
            body: Some(body),
            listen: None,
            session_id: None,
        }
    }
    fn empty(status: u16) -> Self {
        Outcome {
            status,
            body: None,
            listen: None,
            session_id: None,
        }
    }
}

/// An open `subscriptions/listen` stream.
#[derive(Debug, Clone)]
pub struct ListenHandle {
    bus_id: u64,
    bus: Weak<Bus>,
    id: Value,
}

impl ListenHandle {
    /// Detach the subscription.
    pub fn close(&self) {
        if let Some(b) = self.bus.upgrade() {
            b.off(self.bus_id);
        }
    }

    /// The graceful-closure response for a server-ended stream.
    pub fn closure_response(&self) -> Value {
        result_response(
            &self.id,
            json!({"resultType": "complete", "_meta": {META_SUBSCRIPTION_ID: self.id}}),
        )
    }
}

struct Inner {
    store: Arc<dyn DocumentStore>,
    tools: RwLock<BTreeMap<String, ToolDef>>,
    proxy: Option<Arc<Proxy>>,
    bus: Arc<Bus>,
    tasks: Arc<TaskStore>,
    config: ServerConfig,
    instance_id: String,
    /// vcad.app: the session dir node sees. Before a proxied call the app's
    /// document is written here; after it, node's result is committed back
    /// to the live window as one undo step.
    mirror: Option<Arc<crate::store::HeadlessStore>>,
}

/// The MCP server. Cheap to clone.
#[derive(Clone)]
pub struct Server {
    inner: Arc<Inner>,
}

/// How elicitations are answered during one dispatch.
enum ElicitMode {
    /// Legacy: cancel (no bidirectional channel) — or the given live channel.
    Live(Option<ElicitFn>),
    /// Modern synchronous MRTR: answers from `inputResponses`, else capture.
    Mrtr {
        responses: Map<String, Value>,
        captured: Arc<Mutex<(u64, Map<String, Value>)>>,
        allowed: bool,
    },
}

impl Server {
    /// A server over `store` with the native core tools registered.
    pub fn new(store: Arc<dyn DocumentStore>, mut config: ServerConfig) -> Self {
        let bus = Arc::new(Bus::default());
        let tasks = Arc::new(TaskStore::new(bus.clone()));
        let instance_id = format!("{}-{}", crate::store::boot_token(), std::process::id());
        // Share the session dir with the node upstream (the app has none of
        // its own: give node a mirror dir the core keeps in step).
        let mut mirror = None;
        if let Some(p) = config.proxy.as_mut() {
            let dir = store.location().or_else(|| {
                (store.kind() == StoreKind::App).then(|| {
                    crate::store::vcad_home()
                        .join("app-proxy")
                        .join(std::process::id().to_string())
                })
            });
            if let Some(dir) = dir {
                if store.location().is_none() {
                    mirror = Some(Arc::new(crate::store::HeadlessStore::new(dir.clone())));
                }
                p.env
                    .push(("VCAD_MCP_SESSION_DIR".into(), dir.display().to_string()));
            }
            // The core writes session files between node's calls.
            p.env.push(("VCAD_MCP_PROXIED".into(), "1".into()));
        }
        let proxy = config.proxy.clone().map(|c| Arc::new(Proxy::new(c)));
        if let Some(p) = &proxy {
            let weak = Arc::downgrade(p);
            let bus2 = Arc::downgrade(&bus);
            p.set_on_tools_changed(Arc::new(move || {
                if let Some(p) = weak.upgrade() {
                    p.invalidate_tools();
                }
                if let Some(b) = bus2.upgrade() {
                    b.emit(BusEvent::ToolsListChanged);
                }
            }));
        }
        let info = InfoSource {
            name: config.name.clone(),
            version: config.version.clone(),
            started: Instant::now(),
            instance_id: instance_id.clone(),
        };
        let tools = crate::tools::native_tools(info)
            .into_iter()
            .map(|t| (t.name.clone(), t))
            .collect();
        // Store changes → resource updates.
        {
            let bus2 = Arc::downgrade(&bus);
            let store2: Weak<dyn DocumentStore> = Arc::downgrade(&store);
            store.watch(Arc::new(move |evt: &StoreEvent| {
                let Some(bus) = bus2.upgrade() else { return };
                let current = store2.upgrade().and_then(|s| s.current());
                match evt {
                    StoreEvent::Changed(id) | StoreEvent::Closed(id) => {
                        bus.emit(BusEvent::ResourceUpdated(format!("vcad://doc/{id}")));
                        if current.as_ref() == Some(id) {
                            bus.emit(BusEvent::ResourceUpdated("vcad://doc/current".into()));
                        }
                    }
                    StoreEvent::CurrentChanged(_) => {
                        bus.emit(BusEvent::ResourceUpdated("vcad://doc/current".into()))
                    }
                }
            }));
        }
        Server {
            inner: Arc::new(Inner {
                store,
                tools: RwLock::new(tools),
                proxy,
                bus,
                tasks,
                config,
                instance_id,
                mirror,
            }),
        }
    }

    /// Register (or replace) a native tool; fires `toolsListChanged`.
    pub fn register(&self, tool: ToolDef) {
        self.inner
            .tools
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(tool.name.clone(), tool);
        self.inner.bus.emit(BusEvent::ToolsListChanged);
    }

    /// The store.
    pub fn store(&self) -> &Arc<dyn DocumentStore> {
        &self.inner.store
    }

    /// The process event bus.
    pub fn bus(&self) -> &Arc<Bus> {
        &self.inner.bus
    }

    fn server_info(&self) -> Value {
        json!({"name": self.inner.config.name, "version": self.inner.config.version})
    }

    fn capabilities(&self) -> Value {
        json!({
            "tools": {"listChanged": true},
            "resources": {"subscribe": true, "listChanged": true},
            "prompts": {"listChanged": false},
            "completions": {},
            "logging": {},
        })
    }

    fn build_meta(&self) -> Value {
        json!({
            "boot_token": crate::store::boot_token(),
            "instance_id": self.inner.instance_id,
            "session_durable": self.inner.store.durable(),
            "store": self.inner.store.kind().as_str(),
        })
    }

    // ── Shared dispatch ─────────────────────────────────────────────

    fn tools_list(&self) -> Value {
        let native = self.inner.tools.read().unwrap_or_else(|e| e.into_inner());
        let mut list: Vec<Value> = native.values().map(ToolDef::listing).collect();
        if let Some(p) = &self.inner.proxy {
            if let Some(up) = p.list_tools() {
                for t in up {
                    let name = t.get("name").and_then(Value::as_str).unwrap_or("");
                    if !native.contains_key(name) {
                        list.push(t);
                    }
                }
            }
        }
        if let Some(h) = &self.inner.config.listing_hook {
            list.iter_mut().for_each(|t| h(t));
        }
        list.sort_by(|a, b| {
            a.get("name")
                .and_then(Value::as_str)
                .cmp(&b.get("name").and_then(Value::as_str))
        });
        json!({ "tools": list })
    }

    fn is_long_running(&self, name: &str) -> bool {
        self.inner
            .tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .map(|t| t.long_running)
            .unwrap_or_else(|| LONG_RUNNING_TOOLS.contains(&name))
    }

    fn call_tool(&self, params: &Value, mut ctx: ToolCtx) -> Result<Value, RpcErr> {
        // Legacy clients may still send MRTR answers inline.
        if ctx.input_responses.is_null() {
            if let Some(v) = params.get("inputResponses").filter(|v| v.is_object()) {
                ctx.input_responses = v.clone();
            }
        }
        let mut out = self.call_tool_inner(params, ctx)?;
        self.stamp_instance(&mut out);
        Ok(out)
    }

    /// `ServerConfig::instance_meta`: name the serving instance on every tool
    /// result (`_meta["io.vcad/instance"]` + one trailing text line), so an
    /// agent can tell which vcad.app window it is driving.
    fn stamp_instance(&self, out: &mut Value) {
        let Some(f) = &self.inner.config.instance_meta else {
            return;
        };
        let meta = f();
        let Some(obj) = out.as_object_mut() else {
            return;
        };
        let line = match (
            meta.get("pid"),
            meta.get("document").and_then(Value::as_str),
        ) {
            (Some(pid), doc) => Some(format!(
                "[vcad.app pid {pid} · {}]",
                doc.unwrap_or("no document")
            )),
            _ => None,
        };
        if let Some(m) = obj
            .entry("_meta")
            .or_insert_with(|| json!({}))
            .as_object_mut()
        {
            m.insert("io.vcad/instance".into(), meta);
        }
        if let (Some(line), Some(c)) = (line, obj.get_mut("content").and_then(Value::as_array_mut))
        {
            c.push(json!({"type": "text", "text": line}));
        }
    }

    fn call_tool_inner(&self, params: &Value, ctx: ToolCtx) -> Result<Value, RpcErr> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcErr::new(ERR_INVALID_PARAMS, "tools/call: missing name"))?;
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let native = self
            .inner
            .tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned();
        let mut delegated = None;
        if let Some(tool) = native {
            match (tool.handler)(&ctx, args.clone()) {
                Ok(out) => {
                    let mut v = out.to_json();
                    v["_meta"] =
                        json!({ META_VCAD_BUILD: self.build_meta(), "io.vcad/servedBy": "native" });
                    return Ok(v);
                }
                Err(ToolError::InvalidArgs(m)) => {
                    // Tool-level error, as node reports them.
                    let mut v = crate::tools::ToolOutput::error(m).to_json();
                    v["_meta"] = json!({ META_VCAD_BUILD: self.build_meta() });
                    return Ok(v);
                }
                Err(ToolError::Delegate(why)) => delegated = Some(why),
            }
        }
        let Some(proxy) = &self.inner.proxy else {
            return Err(match delegated {
                Some(why) => RpcErr::new(
                    ERR_INVALID_PARAMS,
                    format!("{name}: {why}, and no node upstream is configured"),
                ),
                None => RpcErr::new(ERR_INVALID_PARAMS, format!("Unknown tool '{name}'")),
            });
        };
        let mut args = args;
        let mirrored = self.mirror_in(name, &mut args);
        let result = proxy.call_tool(
            name,
            args,
            Some(ctx.progress.clone()),
            Some(ctx.elicit.clone()),
        );
        if let (Some((id, before)), Some(m)) = (mirrored, &self.inner.mirror) {
            if let Ok(after) = m.load(&id) {
                if serde_json::to_string(&after).ok() != Some(before) {
                    let _ = self.inner.store.commit(&id, after, CommitMeta::agent(name));
                }
            }
        }
        let mut v = result.map_err(|e| RpcErr {
            code: e.code,
            message: e.message,
            data: e.data,
        })?;
        if let Some(obj) = v.as_object_mut() {
            let meta = obj.entry("_meta").or_insert_with(|| json!({}));
            if let Some(m) = meta.as_object_mut() {
                m.insert("io.vcad/servedBy".into(), json!("node-proxy"));
            }
        }
        Ok(v)
    }

    /// vcad.app + proxied call: write the target document (named, else the
    /// front window) into node's session dir and name it in the arguments.
    fn mirror_in(&self, name: &str, args: &mut Value) -> Option<(DocumentId, String)> {
        let m = self.inner.mirror.as_ref()?;
        // Only tools that take a session document.
        let takes_doc = self.inner.proxy.as_ref()?.list_tools()?.iter().any(|t| {
            t.get("name").and_then(Value::as_str) == Some(name)
                && t.pointer("/inputSchema/properties/document_id").is_some()
        });
        if !takes_doc {
            return None;
        }
        let id = match args.get("document_id").and_then(Value::as_str) {
            Some(id) => DocumentId::from(id),
            None => self.inner.store.current()?,
        };
        let doc = self.inner.store.load(&id).ok()?;
        let before = serde_json::to_string(&doc).ok()?;
        m.put(&id, doc).ok()?;
        if let Some(o) = args.as_object_mut() {
            o.entry("document_id").or_insert_with(|| json!(id.0));
        }
        Some((id, before))
    }

    fn resources_list(&self) -> Value {
        let mut res = vec![json!({
            "uri": "vcad://doc/current",
            "name": "Current document",
            "description": "The document the user (app) or the last tool call is working on.",
            "mimeType": "application/vnd.vcad+json",
        })];
        res.extend(self.inner.store.live_resources());
        if let Ok(docs) = self.inner.store.list() {
            for d in docs {
                res.push(json!({
                    "uri": format!("vcad://doc/{}", d.id),
                    "name": d.name.unwrap_or_else(|| d.id.0.clone()),
                    "mimeType": "application/vnd.vcad+json",
                }));
            }
        }
        json!({ "resources": res })
    }

    fn resources_read(&self, params: &Value) -> Result<Value, RpcErr> {
        let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
        if let Some(r) = self.inner.store.read_live_resource(uri) {
            return r.map_err(|e| RpcErr::new(LEGACY_RESOURCE_NOT_FOUND, e.to_string()));
        }
        let not_found = || RpcErr {
            code: LEGACY_RESOURCE_NOT_FOUND,
            message: format!("Resource not found: {uri}"),
            data: Some(json!({"uri": uri})),
        };
        let id = match uri.strip_prefix("vcad://doc/") {
            Some("current") => self.inner.store.current().ok_or_else(not_found)?,
            // Reserved for the app's live resources (M1): never document ids.
            Some(r) if RESERVED_DOC_SEGMENTS.contains(&r) => return Err(not_found()),
            Some(id) => DocumentId::from(id),
            None => return Err(not_found()),
        };
        let doc = self.inner.store.load(&id).map_err(|_| not_found())?;
        let text =
            serde_json::to_string(&doc).map_err(|e| RpcErr::new(ERR_INTERNAL, e.to_string()))?;
        Ok(
            json!({ "contents": [{ "uri": uri, "mimeType": "application/vnd.vcad+json", "text": text, "_meta": {"io.vcad/documentId": id.0} }] }),
        )
    }

    /// Methods both eras share.
    fn dispatch(&self, method: &str, params: &Value, ctx: ToolCtx) -> Result<Value, RpcErr> {
        match method {
            "tools/list" => Ok(self.tools_list()),
            "tools/call" => self.call_tool(params, ctx),
            "resources/list" => Ok(self.resources_list()),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [{
                "uriTemplate": "vcad://doc/{document_id}",
                "name": "Session document",
                "mimeType": "application/vnd.vcad+json",
            }] })),
            "resources/read" => self.resources_read(params),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            "prompts/get" => Err(RpcErr::new(ERR_INVALID_PARAMS, "Unknown prompt")),
            "completion/complete" => {
                Ok(json!({ "completion": { "values": [], "hasMore": false } }))
            }
            _ => Err(RpcErr::new(
                ERR_METHOD_NOT_FOUND,
                format!("Unknown method '{method}'"),
            )),
        }
    }

    fn make_ctx(&self, mode: &ElicitMode, method: &str, progress: ProgressFn) -> ToolCtx {
        let elicit: ElicitFn = match mode {
            ElicitMode::Live(Some(f)) => f.clone(),
            ElicitMode::Live(None) => Arc::new(|_| json!({"action": "cancel"})),
            ElicitMode::Mrtr {
                responses,
                captured,
                allowed,
            } => {
                let (responses, captured, allowed) = (
                    responses.clone(),
                    captured.clone(),
                    *allowed && mrtr_allowed(method),
                );
                Arc::new(move |params: Value| {
                    let mut c = captured.lock().unwrap_or_else(|e| e.into_inner());
                    c.0 += 1;
                    let key = format!("input_{}", c.0);
                    if let Some(r) = responses.get(&key) {
                        return r.clone();
                    }
                    if allowed {
                        let mut clean = params;
                        if let Some(o) = clean.as_object_mut() {
                            o.remove("elicitationId");
                        }
                        c.1.insert(
                            key,
                            json!({"method": "elicitation/create", "params": clean}),
                        );
                    }
                    json!({"action": "cancel"})
                })
            }
        };
        let input_responses = match mode {
            ElicitMode::Mrtr { responses, .. } if !responses.is_empty() => {
                Value::Object(responses.clone())
            }
            _ => Value::Null,
        };
        ToolCtx {
            store: self.inner.store.clone(),
            progress,
            elicit,
            cancel: Arc::new(Default::default()),
            input_responses,
        }
    }

    // ── Modern era ──────────────────────────────────────────────────

    /// Serve one modern message. `sink` receives request-scoped notifications
    /// and, for `subscriptions/listen`, the stream's notifications.
    pub fn handle_modern(
        &self,
        msg: &Value,
        headers: Option<&Headers>,
        sink: Option<Sink>,
    ) -> Outcome {
        if msg.is_array() {
            return Outcome::body(
                400,
                error_response(
                    &Value::Null,
                    ERR_INVALID_PARAMS,
                    "Batched requests are not supported; send one JSON-RPC message per POST",
                    None,
                ),
            );
        }
        if !msg.is_object() {
            return Outcome::body(
                400,
                error_response(
                    &Value::Null,
                    ERR_INVALID_PARAMS,
                    "Malformed JSON-RPC message",
                    None,
                ),
            );
        }
        let id = match msg.get("id") {
            None | Some(Value::Null) => return Outcome::empty(202),
            Some(id) => id.clone(),
        };
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            return Outcome::body(
                400,
                error_response(&id, ERR_INVALID_PARAMS, "Missing JSON-RPC method", None),
            );
        };
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        let meta = meta_of(msg).cloned().unwrap_or_default();

        // Version check.
        let body_version = meta.get(META_PROTOCOL_VERSION).and_then(Value::as_str);
        let header_version = headers.and_then(|h| h.get("mcp-protocol-version"));
        if let (Some(b), Some(h)) = (body_version, header_version) {
            if b != h {
                return header_mismatch(
                    &id,
                    &format!(
                        "MCP-Protocol-Version header value '{h}' does not match body value '{b}'"
                    ),
                );
            }
        }
        match body_version.or(header_version) {
            None if method != "server/discover" => {
                return unsupported_version(&id, "(unspecified)")
            }
            Some(v) if v != PROTOCOL_2026 => return unsupported_version(&id, v),
            _ => {}
        }

        // Header/body agreement (HTTP only).
        if let Some(h) = headers {
            match h.get("mcp-method") {
                None => return header_mismatch(&id, "required header Mcp-Method is missing"),
                Some(m) if m != method => {
                    return header_mismatch(
                        &id,
                        &format!(
                            "Mcp-Method header value '{m}' does not match body value '{method}'"
                        ),
                    )
                }
                _ => {}
            }
            let expected = match method {
                "tools/call" | "prompts/get" => params.get("name").and_then(Value::as_str),
                "resources/read" => params.get("uri").and_then(Value::as_str),
                _ => None,
            };
            if let Some(exp) = expected {
                match h.get("mcp-name") {
                    None => return header_mismatch(&id, "required header Mcp-Name is missing"),
                    Some(n) if decode_header_value(n) != exp => {
                        return header_mismatch(
                            &id,
                            &format!("Mcp-Name header value does not match body value '{exp}'"),
                        )
                    }
                    _ => {}
                }
            }
        }

        if matches!(method, "tasks/get" | "tasks/update" | "tasks/cancel") {
            return match self.inner.tasks.handle(method, &params) {
                Ok(r) => Outcome::body(200, result_response(&id, r)),
                Err(m) => Outcome::body(200, error_response(&id, ERR_INVALID_PARAMS, &m, None)),
            };
        }
        if method == "initialize" {
            return Outcome::body(404, error_response(&id, ERR_METHOD_NOT_FOUND, &format!(
                "'initialize' was removed in protocol {PROTOCOL_2026}; use 'server/discover'. This server supports: {}",
                SUPPORTED_PROTOCOL_VERSIONS.join(", ")
            ), None));
        }
        if method == "subscriptions/listen" {
            let sink = sink.unwrap_or_else(|| Arc::new(|_| {}));
            let handle = self.listen(&id, &params, sink);
            return Outcome {
                status: 200,
                body: None,
                listen: Some(handle),
                session_id: None,
            };
        }
        if method == "ping" {
            return Outcome::body(200, result_response(&id, json!({"resultType": "complete"})));
        }
        if method != "server/discover" && !is_shared_method(method) {
            return Outcome::body(
                404,
                error_response(
                    &id,
                    ERR_METHOD_NOT_FOUND,
                    &format!("Unknown method '{method}'"),
                    None,
                ),
            );
        }

        let server_info = self.server_info();
        if method == "server/discover" {
            let mut caps = self.capabilities();
            caps["extensions"] = json!({ TASKS_EXTENSION: {}, "io.vcad/documents": {} });
            let mut result = json!({
                "resultType": "complete",
                "supportedVersions": SUPPORTED_PROTOCOL_VERSIONS,
                "capabilities": caps,
                "_meta": { META_SERVER_INFO: server_info },
            });
            if let Some(i) = &self.inner.config.instructions {
                result["instructions"] = json!(i);
            }
            decorate_cache(&mut result, method);
            return Outcome::body(200, result_response(&id, result));
        }

        let client_caps = meta
            .get(META_CLIENT_CAPABILITIES)
            .cloned()
            .unwrap_or_else(|| json!({}));
        let wants_tasks = client_caps
            .get("extensions")
            .and_then(|e| e.get(TASKS_EXTENSION))
            .is_some();
        let input_responses = params
            .get("inputResponses")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let forwarded = strip_modern_fields(&params);
        let tool_name = forwarded
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let progress_sink = sink.clone();
        let progress: ProgressFn = Arc::new(move |p, total, message| {
            if let Some(s) = &progress_sink {
                let mut params = json!({"progressToken": "modern", "progress": p});
                if let Some(t) = total {
                    params["total"] = json!(t);
                }
                if let Some(m) = message {
                    params["message"] = json!(m);
                }
                s(json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": params}));
            }
        });

        // Tasks path.
        if method == "tools/call" && wants_tasks && self.is_long_running(&tool_name) {
            let (task_id, cancel, detailed) = self.inner.tasks.create(&tool_name, input_responses);
            let tasks = self.inner.tasks.clone();
            let (tid1, tid2, tid3) = (task_id.clone(), task_id.clone(), task_id.clone());
            let tasks1 = tasks.clone();
            let tasks2 = tasks.clone();
            let name2 = tool_name.clone();
            let ctx = ToolCtx {
                store: self.inner.store.clone(),
                progress: Arc::new(move |p, total, message| {
                    let counter = match total {
                        Some(t) => format!(" ({p}/{t})"),
                        None => format!(" ({p})"),
                    };
                    tasks1.progress(
                        &tid1,
                        format!(
                            "{}{counter}",
                            message.unwrap_or_else(|| format!("running {name2}"))
                        ),
                    );
                }),
                elicit: Arc::new(move |params| tasks2.elicit(&tid2, params)),
                cancel,
                input_responses: Value::Null,
            };
            let server = self.clone();
            std::thread::spawn(move || match server.call_tool(&forwarded, ctx) {
                Ok(r) => tasks.complete(&tid3, r),
                Err(e) => tasks.fail(&tid3, json!({"code": e.code, "message": e.message})),
            });
            let _ = task_id;
            let mut result = detailed;
            result["resultType"] = json!("task");
            result["_meta"] = json!({ META_SERVER_INFO: server_info });
            return Outcome::body(200, result_response(&id, result));
        }

        // Synchronous path with MRTR.
        let captured = Arc::new(Mutex::new((0u64, Map::new())));
        let mode = ElicitMode::Mrtr {
            responses: input_responses,
            captured: captured.clone(),
            allowed: true,
        };
        let ctx = self.make_ctx(&mode, method, progress);
        let reply = self.dispatch(method, &forwarded, ctx);
        let captured = std::mem::take(&mut captured.lock().unwrap_or_else(|e| e.into_inner()).1);
        if !captured.is_empty() {
            return Outcome::body(
                200,
                result_response(
                    &id,
                    json!({
                        "resultType": "input_required",
                        "inputRequests": captured,
                        "_meta": { META_SERVER_INFO: server_info },
                    }),
                ),
            );
        }
        match reply {
            Err(e) => {
                let code = if e.code == LEGACY_RESOURCE_NOT_FOUND {
                    ERR_INVALID_PARAMS
                } else {
                    e.code
                };
                let status = if code == ERR_METHOD_NOT_FOUND {
                    404
                } else {
                    200
                };
                Outcome::body(status, error_response(&id, code, &e.message, e.data))
            }
            Ok(mut result) => {
                result["resultType"] = json!("complete");
                let meta = result.as_object_mut().and_then(|o| {
                    o.entry("_meta").or_insert_with(|| json!({}));
                    o.get_mut("_meta")
                });
                if let Some(m) = meta.and_then(Value::as_object_mut) {
                    m.insert(META_SERVER_INFO.into(), server_info);
                }
                decorate_cache(&mut result, method);
                Outcome::body(200, result_response(&id, result))
            }
        }
    }

    /// `subscriptions/listen`: ack, then route opted-in events to `sink`.
    fn listen(&self, id: &Value, params: &Value, sink: Sink) -> ListenHandle {
        let filter = params
            .get("notifications")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut honored = Map::new();
        let tools_changed = filter.get("toolsListChanged") == Some(&Value::Bool(true));
        if tools_changed {
            honored.insert("toolsListChanged".into(), json!(true));
        }
        let task_ids: HashSet<String> = filter
            .get("taskIds")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if !task_ids.is_empty() {
            honored.insert("taskIds".into(), json!(task_ids.iter().collect::<Vec<_>>()));
        }
        let uris: HashSet<String> = filter
            .get("resourceSubscriptions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if !uris.is_empty() {
            honored.insert(
                "resourceSubscriptions".into(),
                json!(uris.iter().collect::<Vec<_>>()),
            );
        }
        if filter.get("resourcesListChanged") == Some(&Value::Bool(true)) {
            honored.insert("resourcesListChanged".into(), json!(true));
        }
        let resources_list = honored.contains_key("resourcesListChanged");
        // Ack MUST precede every notification on this subscription.
        sink(json!({
            "jsonrpc": "2.0",
            "method": "notifications/subscriptions/acknowledged",
            "params": { "_meta": { META_SUBSCRIPTION_ID: id }, "notifications": honored },
        }));
        let sub = id.clone();
        let bus_id = self.inner.bus.on(Arc::new(move |evt| {
            let meta = json!({ META_SUBSCRIPTION_ID: sub });
            match evt {
                BusEvent::ToolsListChanged if tools_changed => sink(json!({
                    "jsonrpc": "2.0", "method": "notifications/tools/list_changed", "params": { "_meta": meta },
                })),
                BusEvent::Task(t) => {
                    if t.get("taskId").and_then(Value::as_str).is_some_and(|tid| task_ids.contains(tid)) {
                        let mut p = t.clone();
                        p["_meta"] = meta;
                        sink(json!({"jsonrpc": "2.0", "method": "notifications/tasks", "params": p}));
                    }
                }
                BusEvent::ResourceUpdated(uri) => {
                    if uris.contains(uri) {
                        sink(json!({"jsonrpc": "2.0", "method": "notifications/resources/updated", "params": {"uri": uri, "_meta": meta.clone()}}));
                    }
                    if resources_list && uri != "vcad://doc/current" {
                        sink(json!({"jsonrpc": "2.0", "method": "notifications/resources/list_changed", "params": {"_meta": meta}}));
                    }
                }
                _ => {}
            }
        }));
        ListenHandle {
            bus_id,
            bus: Arc::downgrade(&self.inner.bus),
            id: id.clone(),
        }
    }
}

/// `vcad://doc/<segment>` names that are not document ids (M1 owns them).
pub const RESERVED_DOC_SEGMENTS: &[&str] = &["current", "selection", "view"];

fn decorate_cache(result: &mut Value, method: &str) {
    if let Some(ttl) = cache_ttl(method) {
        result["ttlMs"] = json!(ttl);
        result["cacheScope"] = json!("private");
    }
}

fn header_mismatch(id: &Value, message: &str) -> Outcome {
    Outcome::body(
        400,
        error_response(
            id,
            ERR_HEADER_MISMATCH,
            &format!("Header mismatch: {message}"),
            None,
        ),
    )
}

fn unsupported_version(id: &Value, requested: &str) -> Outcome {
    Outcome::body(
        400,
        error_response(
            id,
            ERR_UNSUPPORTED_PROTOCOL_VERSION,
            "Unsupported protocol version",
            Some(json!({
                "supported": SUPPORTED_PROTOCOL_VERSIONS,
                "requested": requested,
            })),
        ),
    )
}

// ── Connections (legacy state + routing) ─────────────────────────────

#[derive(Default)]
struct LegacyState {
    initialized: bool,
    version: Option<String>,
    client_elicitation: bool,
    subscriptions: HashSet<String>,
}

/// One client connection (a stdio pipe, a unix socket stream, or a single
/// HTTP exchange). Holds the legacy handshake state, the connection's listen
/// streams and pending server→client requests.
pub struct Connection {
    server: Server,
    sink: Sink,
    state: Arc<Mutex<LegacyState>>,
    pending: Arc<Mutex<HashMap<String, Sender<Value>>>>,
    next_req: AtomicU64,
    listens: Mutex<Vec<ListenHandle>>,
    bus_id: u64,
    /// HTTP exchanges are one-shot: legacy requests are accepted without a
    /// prior `initialize` on the same connection.
    lenient: bool,
}

impl Connection {
    /// A persistent connection writing to `sink`.
    pub fn new(server: Server, sink: Sink) -> Arc<Self> {
        Self::build(server, sink, false)
    }

    /// A one-shot HTTP exchange.
    pub fn one_shot(server: Server, sink: Sink) -> Arc<Self> {
        Self::build(server, sink, true)
    }

    fn build(server: Server, sink: Sink, lenient: bool) -> Arc<Self> {
        let state: Arc<Mutex<LegacyState>> = Arc::default();
        let bus_id = if lenient {
            u64::MAX
        } else {
            let (st, sk) = (Arc::downgrade(&state), sink.clone());
            server.inner.bus.on(Arc::new(move |evt| {
                let Some(st) = st.upgrade() else { return };
                let st = st.lock().unwrap_or_else(|e| e.into_inner());
                if !st.initialized {
                    return;
                }
                match evt {
                    BusEvent::ToolsListChanged => sk(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"})),
                    BusEvent::ResourceUpdated(uri) if st.subscriptions.contains(uri) => {
                        sk(json!({"jsonrpc":"2.0","method":"notifications/resources/updated","params":{"uri": uri}}))
                    }
                    _ => {}
                }
            }))
        };
        Arc::new(Connection {
            server,
            sink,
            state,
            pending: Arc::default(),
            next_req: AtomicU64::new(1),
            listens: Mutex::new(Vec::new()),
            bus_id,
            lenient,
        })
    }

    /// Send a request to the client and wait for its response (legacy
    /// elicitation). `None` on timeout or a closed connection.
    pub fn request_client(&self, method: &str, params: Value, timeout: Duration) -> Option<Value> {
        let id = format!("srv-{}", self.next_req.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), tx);
        (self.sink)(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        let r = rx.recv_timeout(timeout).ok();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        r
    }

    /// Handle one inbound message (either era). Returns the response to write
    /// (if any); notifications go to the sink as they happen.
    pub fn handle(self: &Arc<Self>, msg: Value, headers: Option<&Headers>) -> Outcome {
        // A response to one of our server→client requests.
        if msg.get("method").is_none() {
            if let Some(id) = msg.get("id").map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            }) {
                if let Some(tx) = self
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id)
                {
                    let _ = tx.send(
                        msg.get("result")
                            .cloned()
                            .unwrap_or_else(|| json!({"action": "cancel"})),
                    );
                    return Outcome::empty(202);
                }
            }
        }
        if is_modern_message(&msg, headers) {
            let out = self
                .server
                .handle_modern(&msg, headers, Some(self.sink.clone()));
            if let Some(l) = &out.listen {
                self.listens
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(l.clone());
            }
            return out;
        }
        self.handle_legacy(&msg)
    }

    fn handle_legacy(self: &Arc<Self>, msg: &Value) -> Outcome {
        if msg.is_array() {
            return Outcome::body(
                400,
                error_response(
                    &Value::Null,
                    ERR_INVALID_REQUEST,
                    "Batched requests are not supported",
                    None,
                ),
            );
        }
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            return Outcome::body(
                400,
                error_response(
                    msg.get("id").unwrap_or(&Value::Null),
                    ERR_INVALID_REQUEST,
                    "Invalid request",
                    None,
                ),
            );
        };
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        let Some(id) = msg.get("id").filter(|v| !v.is_null()).cloned() else {
            if method == "notifications/initialized" {
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .initialized = true;
            }
            return Outcome::empty(202);
        };
        let ok = |r: Value| Outcome::body(200, result_response(&id, r));
        let err = |e: RpcErr| Outcome::body(200, error_response(&id, e.code, &e.message, e.data));
        match method {
            "initialize" => {
                let requested = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(LATEST_LEGACY);
                let version = if LEGACY_VERSIONS.contains(&requested) {
                    requested
                } else {
                    LATEST_LEGACY
                };
                {
                    let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    st.version = Some(version.to_string());
                    st.client_elicitation = params
                        .get("capabilities")
                        .and_then(|c| c.get("elicitation"))
                        .is_some();
                    // Lenient (HTTP) or clients that skip the notification.
                    st.initialized = true;
                }
                let mut result = json!({
                    "protocolVersion": version,
                    "capabilities": self.server.capabilities(),
                    "serverInfo": self.server.server_info(),
                });
                if let Some(i) = &self.server.inner.config.instructions {
                    result["instructions"] = json!(i);
                }
                let mut out = ok(result);
                out.session_id = Some(format!(
                    "mcp-{}",
                    crate::store::mint_document_id()
                        .0
                        .trim_start_matches("doc_")
                ));
                out
            }
            "ping" => ok(json!({})),
            "logging/setLevel" => ok(json!({})),
            "resources/subscribe" | "resources/unsubscribe" => {
                let uri = params
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if method == "resources/subscribe" {
                    st.subscriptions.insert(uri);
                } else {
                    st.subscriptions.remove(&uri);
                }
                ok(json!({}))
            }
            m if is_shared_method(m) => {
                if !self.lenient
                    && !self
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .initialized
                {
                    return err(RpcErr::new(
                        ERR_INVALID_REQUEST,
                        "Server not initialized: send 'initialize' first",
                    ));
                }
                let elicit: Option<ElicitFn> = if !self.lenient
                    && self
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .client_elicitation
                {
                    let weak = Arc::downgrade(self);
                    Some(Arc::new(move |p| {
                        weak.upgrade()
                            .and_then(|c| {
                                c.request_client("elicitation/create", p, Duration::from_secs(600))
                            })
                            .unwrap_or_else(|| json!({"action": "cancel"}))
                    }))
                } else {
                    None
                };
                let token = params
                    .get("_meta")
                    .and_then(|m| m.get("progressToken"))
                    .cloned();
                let sink = self.sink.clone();
                let progress: ProgressFn = Arc::new(move |p, total, message| {
                    let Some(tok) = &token else { return };
                    let mut params = json!({"progressToken": tok, "progress": p});
                    if let Some(t) = total {
                        params["total"] = json!(t);
                    }
                    if let Some(m) = message {
                        params["message"] = json!(m);
                    }
                    sink(
                        json!({"jsonrpc":"2.0","method":"notifications/progress","params":params}),
                    );
                });
                let ctx = self.server.make_ctx(&ElicitMode::Live(elicit), m, progress);
                match self.server.dispatch(m, &params, ctx) {
                    Ok(r) => ok(r),
                    Err(e) => err(e),
                }
            }
            "server/discover" | "subscriptions/listen" => {
                unreachable!("modern-only methods route to handle_modern")
            }
            _ => err(RpcErr::new(
                ERR_METHOD_NOT_FOUND,
                format!("Unknown method '{method}'"),
            )),
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if self.bus_id != u64::MAX {
            self.server.inner.bus.off(self.bus_id);
        }
        for l in self
            .listens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            l.close();
        }
    }
}

/// Convenience: the store kind a server runs on.
pub fn store_kind(server: &Server) -> StoreKind {
    server.inner.store.kind()
}
