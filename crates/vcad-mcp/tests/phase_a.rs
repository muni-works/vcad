//! Phase A: one tool implementation behind both stores.
//!
//! - Tool parity: every native / host tool in `docs/launch/mcp-tool-inventory.md`
//!   is listed and callable through the app store and through headless, with
//!   the same listing; every proxied tool is one the node server has.
//! - AppStore edit round trip over a fake app bridge: engine edits, loon,
//!   parameters, receipts, undo, host dispatch, MRTR pass-through, resources.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vcad_ir::Document;
use vcad_mcp::store::mint_document_id;
use vcad_mcp::{
    AppBridge, AppStore, CommitMeta, Connection, DocumentId, DocumentSummary, HeadlessStore,
    Server, ServerConfig, StoreError, StoreEvent, StoreListener,
};

// ── Fake vcad.app ────────────────────────────────────────────────────

#[derive(Default)]
struct FakeApp {
    docs: Mutex<BTreeMap<DocumentId, (Document, Vec<(Document, String)>)>>,
    front: Mutex<Option<DocumentId>>,
    applied: Mutex<Vec<String>>,
    host_calls: Mutex<Vec<(String, Value, Value)>>,
    listeners: Mutex<Vec<StoreListener>>,
    confirm: Mutex<String>,
}

impl FakeApp {
    fn emit(&self, e: StoreEvent) {
        for l in self.listeners.lock().unwrap().iter() {
            l(&e);
        }
    }
}

impl AppBridge for FakeApp {
    fn front_document(&self) -> Option<(DocumentId, Document)> {
        let id = self.front.lock().unwrap().clone()?;
        let d = self.docs.lock().unwrap().get(&id)?.0.clone();
        Some((id, d))
    }
    fn open(&self, doc: Document, _name: Option<&str>) -> Result<DocumentId, StoreError> {
        let id = mint_document_id();
        self.docs.lock().unwrap().insert(id.clone(), (doc, vec![]));
        *self.front.lock().unwrap() = Some(id.clone());
        Ok(id)
    }
    fn read(&self, id: &DocumentId) -> Result<Document, StoreError> {
        self.docs
            .lock()
            .unwrap()
            .get(id)
            .map(|d| d.0.clone())
            .ok_or_else(|| StoreError::NotFound(id.clone()))
    }
    fn apply(&self, id: &DocumentId, doc: Document, meta: &CommitMeta) -> Result<(), StoreError> {
        {
            let mut docs = self.docs.lock().unwrap();
            let e = docs
                .get_mut(id)
                .ok_or_else(|| StoreError::NotFound(id.clone()))?;
            let prev = std::mem::replace(&mut e.0, doc);
            e.1.push((prev, meta.label.clone()));
        }
        self.applied.lock().unwrap().push(meta.label.clone());
        self.emit(StoreEvent::Changed(id.clone()));
        Ok(())
    }
    fn release(&self, id: &DocumentId) -> Result<(), StoreError> {
        self.docs.lock().unwrap().remove(id);
        Ok(())
    }
    fn documents(&self) -> Vec<DocumentSummary> {
        self.docs
            .lock()
            .unwrap()
            .keys()
            .map(|id| DocumentSummary {
                id: id.clone(),
                name: None,
            })
            .collect()
    }
    fn watch(&self, listener: StoreListener) {
        self.listeners.lock().unwrap().push(listener);
    }
    fn undo(&self, id: &DocumentId) -> Result<String, StoreError> {
        let mut docs = self.docs.lock().unwrap();
        let e = docs
            .get_mut(id)
            .ok_or_else(|| StoreError::NotFound(id.clone()))?;
        let (prev, label) =
            e.1.pop()
                .ok_or_else(|| StoreError::Unavailable("Nothing to undo".into()))?;
        e.0 = prev;
        Ok(label)
    }
    fn live_resources(&self) -> Vec<Value> {
        vec![
            json!({"uri": "vcad://doc/selection", "name": "Selection", "mimeType": "application/json"}),
        ]
    }
    fn read_live_resource(&self, uri: &str) -> Option<Result<Value, StoreError>> {
        (uri == "vcad://doc/selection").then(|| {
            Ok(json!({"contents": [{"uri": uri, "mimeType": "application/json", "text": "{\"edges\":[]}"}]}))
        })
    }
    fn host_call(
        &self,
        name: &str,
        args: &Value,
        input_responses: &Value,
    ) -> Option<Result<Value, StoreError>> {
        self.host_calls
            .lock()
            .unwrap()
            .push((name.into(), args.clone(), input_responses.clone()));
        if name == vcad_mcp::tools::host::CONFIRM_DESTRUCTIVE {
            return Some(Ok(json!({"action": self.confirm.lock().unwrap().clone()})));
        }
        Some(Ok(
            json!({"content": [{"type": "text", "text": format!("host:{name}")}]}),
        ))
    }
}

// ── Harness ──────────────────────────────────────────────────────────

struct H {
    conn: Arc<Connection>,
    next: Mutex<u64>,
}

impl H {
    fn new(server: Server) -> Self {
        let conn = Connection::new(server, Arc::new(|_| {}));
        let h = H {
            conn,
            next: Mutex::new(1),
        };
        h.send("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}));
        h.conn.handle(
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            None,
        );
        h
    }
    fn send(&self, method: &str, params: Value) -> Value {
        let id = {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            *n
        };
        self.conn
            .handle(
                json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
                None,
            )
            .body
            .expect("body")
    }
    fn call(&self, name: &str, args: Value) -> Value {
        self.send("tools/call", json!({"name": name, "arguments": args}))
    }
    fn ok(&self, name: &str, args: Value) -> Value {
        let r = self.call(name, args.clone());
        assert!(r.get("error").is_none(), "{name} {args}: {r}");
        assert_ne!(r["result"]["isError"], json!(true), "{name} {args}: {r}");
        r["result"].clone()
    }
    fn tool_names(&self) -> BTreeSet<String> {
        self.send("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }
}

fn no_proxy() -> ServerConfig {
    ServerConfig {
        proxy: None,
        ..ServerConfig::default()
    }
}

fn headless() -> (H, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (
        H::new(Server::new(
            Arc::new(HeadlessStore::new(dir.path())),
            no_proxy(),
        )),
        dir,
    )
}

fn app() -> (H, Arc<FakeApp>) {
    let fake = Arc::new(FakeApp::default());
    *fake.confirm.lock().unwrap() = "accept".into();
    fake.open(Document::default(), Some("Untitled")).unwrap();
    let store = AppStore::with_bridge(fake.clone());
    (H::new(Server::new(Arc::new(store), no_proxy())), fake)
}

/// `(tool, after)` rows of the inventory table.
fn inventory() -> Vec<(String, String)> {
    let md = include_str!("../../../docs/launch/mcp-tool-inventory.md");
    md.lines()
        .filter(|l| l.starts_with("| `"))
        .map(|l| {
            let cols: Vec<&str> = l.split('|').map(str::trim).collect();
            (cols[1].trim_matches('`').to_string(), cols[5].to_string())
        })
        .collect()
}

fn node_fixture() -> BTreeSet<String> {
    let raw = include_str!("../../../packages/mcp/src/__tests__/tool-surface.fixture.json");
    let v: Vec<Value> = serde_json::from_str(raw).unwrap();
    v.iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

// ── Parity ───────────────────────────────────────────────────────────

#[test]
fn tool_parity_app_and_headless() {
    let rows = inventory();
    assert!(rows.len() > 200, "inventory parsed {} rows", rows.len());
    let native: BTreeSet<String> = rows
        .iter()
        .filter(|(_, a)| !a.starts_with("node proxy"))
        .map(|(n, _)| n.clone())
        .collect();
    let proxied: BTreeSet<String> = rows
        .iter()
        .filter(|(_, a)| a.starts_with("node proxy"))
        .map(|(n, _)| n.clone())
        .collect();
    // Everything proxied is a node tool: the proxy can serve it unchanged.
    let node = node_fixture();
    let missing: Vec<_> = proxied.difference(&node).collect();
    assert!(missing.is_empty(), "proxied but not in node: {missing:?}");

    let (hl, _dir) = headless();
    let (ap, _fake) = app();
    let hl_tools = hl.tool_names();
    let ap_tools = ap.tool_names();
    assert_eq!(
        hl_tools, ap_tools,
        "app and headless must list the same native tools"
    );
    let unlisted: Vec<_> = native.difference(&hl_tools).collect();
    assert!(
        unlisted.is_empty(),
        "inventory says native/host but not listed: {unlisted:?}"
    );
    let undocumented: Vec<_> = hl_tools.difference(&native).collect();
    assert!(
        undocumented.is_empty(),
        "listed but missing from the inventory: {undocumented:?}"
    );

    // Callable: every tool answers through both stores — a result (possibly a
    // tool-level isError for missing args) or, headless with no node, a clear
    // delegation error. Never "Unknown tool".
    for name in &native {
        for (mode, h) in [("headless", &hl), ("app", &ap)] {
            let r = h.call(name, json!({}));
            let msg = r["error"]["message"].as_str().unwrap_or("");
            assert!(!msg.contains("Unknown tool"), "{mode} {name}: {r}");
            assert!(
                r.get("result").is_some() || msg.contains("no node upstream"),
                "{mode} {name}: {r}"
            );
        }
    }
}

#[test]
fn host_tools_dispatch_to_the_app_and_explain_headless() {
    let (ap, fake) = app();
    let r = ap.ok("set_camera", json!({"azimuth": 30}));
    assert_eq!(r["content"][0]["text"], "host:set_camera");
    // MRTR answers reach the host (the viewport pick uses them over its UI).
    let r = ap.send(
        "tools/call",
        json!({"name": "pick_in_viewport", "arguments": {"message": "which edge?", "kind": "edge"},
               "inputResponses": {"input_1": {"action": "accept", "content": {"selection": []}}}}),
    );
    assert!(r.get("result").is_some(), "{r}");
    let calls = fake.host_calls.lock().unwrap();
    let (_, args, inputs) = calls
        .iter()
        .find(|(n, _, _)| n == "pick_in_viewport")
        .unwrap();
    assert_eq!(args["kind"], "edge");
    assert_eq!(inputs["input_1"]["action"], "accept");
    drop(calls);

    let (hl, _d) = headless();
    let r = hl.call("set_camera", json!({}));
    assert_eq!(r["result"]["isError"], json!(true));
    assert!(r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("vcad.app"));
}

// ── AppStore round trip ──────────────────────────────────────────────

#[test]
fn app_store_edit_round_trip() {
    let (ap, fake) = app();
    let front = fake.front.lock().unwrap().clone().unwrap();

    // Engine edits land in the open window, one named step each.
    let r = ap.ok(
        "create",
        json!({"type": "cube", "name": "Block", "params": {"size": {"x": 20, "y": 10, "z": 4}}}),
    );
    assert_eq!(r["structuredContent"]["document_id"], front.0);
    let block = r["structuredContent"]["created_part_id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = ap.ok(
        "create",
        json!({"type": "cylinder", "params": {"radius": 2, "height": 20}}),
    );
    let pin = r["structuredContent"]["created_part_id"]
        .as_str()
        .unwrap()
        .to_string();
    ap.ok(
        "difference",
        json!({"left": block, "right": pin, "name": "Hole"}),
    );
    assert_eq!(
        *fake.applied.lock().unwrap(),
        vec!["Create Block", "Create Cylinder", "Difference Hole"]
    );
    let doc = fake.read(&front).unwrap();
    assert!(!doc.roots.is_empty());

    // Reads see the live document (no document_id = the front window).
    let text = ap.ok("describe_scene", json!({}))["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!text.is_empty());
    let got = ap.ok("get_document", json!({}));
    let got: Value = serde_json::from_str(got["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(got["roots"], serde_json::to_value(&doc).unwrap()["roots"]);
    let r = ap.send("resources/read", json!({"uri": "vcad://doc/current"}));
    assert!(r["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("nodes"));
    let r = ap.send("resources/read", json!({"uri": "vcad://doc/selection"}));
    assert_eq!(r["result"]["contents"][0]["text"], "{\"edges\":[]}");

    // Loon replaces the open window's document (not a new window), with the
    // agent's undo name; parameters then set as one step.
    ap.ok("create_cad_loon", json!({"source": "[defparam t 5.0 :min 2 :max 12] [pub let plate [cube 40.0 20.0 t]]", "name": "Plate"}));
    assert_eq!(fake.documents().len(), 1);
    assert_eq!(fake.applied.lock().unwrap().last().unwrap(), "Plate");
    let params = ap.ok("list_parameters", json!({}));
    assert_eq!(params["structuredContent"]["parameters"][0]["name"], "t");
    let r = ap.ok("set_parameters", json!({"values": {"t": 99}}));
    assert_eq!(
        r["structuredContent"]["applied"]["t"], 12.0,
        "clamped to :max"
    );
    assert_eq!(fake.applied.lock().unwrap().last().unwrap(), "Set t");

    // Receipts persist on the live document.
    let r = ap.ok("verify_receipts", json!({}));
    assert_eq!(r["structuredContent"]["status"], Value::Null);

    // Undo is the app's ⌘Z.
    let r = ap.ok("undo", json!({}));
    assert_eq!(r["structuredContent"]["undone"], "Set t");
    let p = fake.read(&front).unwrap().parameters["t"].value.clone();
    assert_eq!(serde_json::to_value(p).unwrap(), json!(5.0));
}

#[test]
fn delete_on_a_receipted_document_asks_the_user() {
    let (ap, fake) = app();
    ap.ok("create_cad_loon", json!({"source": "[pub let a [cube 10.0 10.0 10.0]] [pub let b [translate 30.0 0.0 0.0 [cube 10.0 10.0 10.0]]]"}));
    ap.ok(
        "assert_clearance",
        json!({"label": "gap", "group_a": ["a"], "group_b": ["b"], "min_mm": 5}),
    );
    let before = fake
        .read(&fake.front.lock().unwrap().clone().unwrap())
        .unwrap();
    assert_eq!(before.clearance_specs.len(), 1);
    *fake.confirm.lock().unwrap() = "decline".into();
    let r = ap.call("delete", json!({"part_id": "a"}));
    assert_eq!(r["result"]["isError"], json!(true), "{r}");
    assert!(r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("declined"));
    let after = fake
        .read(&fake.front.lock().unwrap().clone().unwrap())
        .unwrap();
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(&after).unwrap()
    );
}

#[test]
fn instance_meta_stamps_every_result() {
    let fake = Arc::new(FakeApp::default());
    fake.open(Document::default(), None).unwrap();
    let mut cfg = no_proxy();
    cfg.instance_meta = Some(Arc::new(|| json!({"pid": 42, "document": "bracket.vcad"})));
    let h = H::new(Server::new(Arc::new(AppStore::with_bridge(fake)), cfg));
    let r = h.ok("server_info", json!({}));
    assert_eq!(r["_meta"]["io.vcad/instance"]["pid"], 42);
    let last = r["content"].as_array().unwrap().last().unwrap()["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(last, "[vcad.app pid 42 · bracket.vcad]");
}

#[test]
fn headless_engine_edits_and_undo() {
    let (hl, _d) = headless();
    let r = hl.ok("create", json!({"type": "sphere", "params": {"radius": 3}}));
    let id = r["structuredContent"]["document_id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = hl.ok("inspect_cad", json!({"document_id": id}));
    assert!(r["structuredContent"]["volume"].as_f64().unwrap() > 100.0);
    hl.ok(
        "create",
        json!({"document_id": id, "type": "cube", "params": {"size": {"x": 1, "y": 1, "z": 1}}}),
    );
    assert_eq!(
        hl.ok("undo", json!({"document_id": id}))["structuredContent"]["undone"],
        "Create Cube"
    );
}

#[test]
fn proxied_tools_edit_the_live_app_document() {
    // VCAD_HOME scopes the app's node mirror dir to this test.
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("VCAD_HOME", home.path());
    let fake = Arc::new(FakeApp::default());
    fake.open(Document::default(), None).unwrap();
    let front = fake.front.lock().unwrap().clone().unwrap();
    let cfg = ServerConfig {
        proxy: Some(vcad_mcp::proxy::ProxyConfig {
            program: env!("CARGO_BIN_EXE_vcad-mcp").into(),
            args: vec![],
            env: vec![("VCAD_MCP_TEST_UPSTREAM".into(), "1".into())],
        }),
        ..ServerConfig::default()
    };
    let h = H::new(Server::new(
        Arc::new(AppStore::with_bridge(fake.clone())),
        cfg,
    ));
    assert!(h.tool_names().contains("upstream_touch"));
    // No document_id: the core names the front window and mirrors it to node.
    let r = h.ok("upstream_touch", json!({}));
    assert_eq!(r["_meta"]["io.vcad/servedBy"], "node-proxy");
    assert_eq!(
        fake.applied.lock().unwrap().last().unwrap(),
        "upstream_touch"
    );
    assert!(fake
        .read(&front)
        .unwrap()
        .parameters
        .contains_key("touched"));
    // Tools that take no document are forwarded untouched.
    let r = h.ok("upstream_echo", json!({"x": 1}));
    assert!(r["structuredContent"]["echo"].get("document_id").is_none());
}
