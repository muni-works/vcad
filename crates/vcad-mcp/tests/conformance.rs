//! Protocol conformance (both eras), core tools against the headless store,
//! the node-proxy round trip, and the HTTP transport.

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use vcad_mcp::protocol::*;
use vcad_mcp::proxy::ProxyConfig;
use vcad_mcp::{Connection, HeadlessStore, Server, ServerConfig, ToolDef, ToolOutput};

struct Harness {
    server: Server,
    conn: Arc<Connection>,
    notes: Arc<Mutex<Vec<Value>>>,
    _dir: tempfile::TempDir,
}

fn harness_with(proxy: Option<ProxyConfig>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HeadlessStore::new(dir.path()));
    let server = Server::new(
        store,
        ServerConfig {
            proxy,
            ..ServerConfig::default()
        },
    );
    let notes: Arc<Mutex<Vec<Value>>> = Arc::default();
    let n2 = notes.clone();
    let conn = Connection::new(
        server.clone(),
        Arc::new(move |m| n2.lock().unwrap().push(m)),
    );
    Harness {
        server,
        conn,
        notes,
        _dir: dir,
    }
}

fn harness() -> Harness {
    harness_with(None)
}

fn modern(id: u64, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({
        META_PROTOCOL_VERSION: PROTOCOL_2026,
        META_CLIENT_INFO: {"name": "test", "version": "1"},
        META_CLIENT_CAPABILITIES: {"extensions": {TASKS_EXTENSION: {}}},
    });
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn legacy(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

impl Harness {
    fn send(&self, msg: Value) -> Value {
        self.conn.handle(msg, None).body.expect("response body")
    }
    fn init_legacy(&self) {
        let r = self.send(legacy(0, "initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}})));
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        self.conn.handle(
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            None,
        );
    }
    fn call(&self, name: &str, args: Value) -> Value {
        let r = self.send(modern(
            99,
            "tools/call",
            json!({"name": name, "arguments": args}),
        ));
        assert!(r.get("error").is_none(), "{name}: {r}");
        r["result"].clone()
    }
}

// ── Legacy era ────────────────────────────────────────────────────────

#[test]
fn legacy_handshake_and_listing() {
    let h = harness();
    let pre = h.send(legacy(1, "tools/list", json!({})));
    assert_eq!(
        pre["error"]["code"], ERR_INVALID_REQUEST,
        "calls before initialize are refused"
    );
    let r = h.send(legacy(
        2,
        "initialize",
        json!({"protocolVersion": "1999-01-01", "capabilities": {}}),
    ));
    assert_eq!(
        r["result"]["protocolVersion"], LATEST_LEGACY,
        "unknown version negotiates down to latest legacy"
    );
    assert_eq!(r["result"]["serverInfo"]["name"], "vcad");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    let tools = h.send(legacy(3, "tools/list", json!({})));
    let list = tools["result"]["tools"].as_array().unwrap();
    let inspect = list.iter().find(|t| t["name"] == "inspect_cad").unwrap();
    assert_eq!(inspect["outputSchema"]["type"], "object");
    assert!(
        tools["result"].get("resultType").is_none(),
        "legacy results are not modern-shaped"
    );
    assert_eq!(h.send(legacy(4, "ping", json!({})))["result"], json!({}));
    assert_eq!(
        h.send(legacy(5, "nope/nope", json!({})))["error"]["code"],
        ERR_METHOD_NOT_FOUND
    );
}

#[test]
fn legacy_tool_call_and_resource_subscription() {
    let h = harness();
    h.init_legacy();
    let r = h.send(legacy(
        1,
        "tools/call",
        json!({"name": "open_document", "arguments": {}}),
    ));
    let id = r["result"]["structuredContent"]["document_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        r["result"]["_meta"][META_VCAD_BUILD]["session_durable"],
        true
    );
    h.send(legacy(
        2,
        "resources/subscribe",
        json!({"uri": format!("vcad://doc/{id}")}),
    ));
    h.notes.lock().unwrap().clear();
    h.send(legacy(3, "tools/call", json!({"name": "create_cad_loon", "arguments": {"document_id": id, "source": "[cube 1 1 1]"}})));
    let notes = h.notes.lock().unwrap();
    assert!(
        notes
            .iter()
            .any(|n| n["method"] == "notifications/resources/updated"
                && n["params"]["uri"] == format!("vcad://doc/{id}")),
        "{notes:?}"
    );
}

// ── Modern era ────────────────────────────────────────────────────────

#[test]
fn modern_discover_and_versioning() {
    let h = harness();
    let d = h.send(json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover"}));
    let r = &d["result"];
    assert_eq!(r["resultType"], "complete");
    assert_eq!(r["supportedVersions"][0], PROTOCOL_2026);
    assert!(r["capabilities"]["extensions"][TASKS_EXTENSION].is_object());
    assert_eq!(r["_meta"][META_SERVER_INFO]["name"], "vcad");
    assert_eq!(r["ttlMs"], DISCOVER_TTL_MS);

    let mut bad = modern(2, "tools/list", json!({}));
    bad["params"]["_meta"][META_PROTOCOL_VERSION] = json!("2030-01-01");
    let e = h.send(bad);
    assert_eq!(e["error"]["code"], ERR_UNSUPPORTED_PROTOCOL_VERSION);
    assert_eq!(e["error"]["data"]["requested"], "2030-01-01");

    let mut init = modern(3, "initialize", json!({}));
    init["params"]["protocolVersion"] = json!("2025-06-18");
    let out = h.conn.handle(init, None);
    assert_eq!(out.status, 404);
    assert!(out.body.unwrap()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("server/discover"));

    let tl = h.send(modern(4, "tools/list", json!({})));
    assert_eq!(tl["result"]["resultType"], "complete");
    assert_eq!(tl["result"]["cacheScope"], "private");
    assert!(h.conn.handle(json!({"jsonrpc": "2.0", "method": "x", "params": {"_meta": {META_PROTOCOL_VERSION: PROTOCOL_2026}}}), None).body.is_none(), "notifications get no body");
}

#[test]
fn modern_http_header_agreement() {
    let h = harness();
    let msg = modern(
        1,
        "tools/call",
        json!({"name": "server_info", "arguments": {}}),
    );
    let hdr = |pairs: &[(&str, &str)]| {
        Headers(
            pairs
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        )
    };
    let missing = h.server.handle_modern(&msg, Some(&hdr(&[])), None);
    assert_eq!(missing.status, 400);
    assert_eq!(missing.body.unwrap()["error"]["code"], ERR_HEADER_MISMATCH);
    let wrong = h.server.handle_modern(
        &msg,
        Some(&hdr(&[("Mcp-Method", "tools/call"), ("Mcp-Name", "other")])),
        None,
    );
    assert_eq!(wrong.body.unwrap()["error"]["code"], ERR_HEADER_MISMATCH);
    let good = h.server.handle_modern(
        &msg,
        Some(&hdr(&[
            ("mcp-method", "tools/call"),
            ("Mcp-Name", "=?base64?c2VydmVyX2luZm8=?="),
        ])),
        None,
    );
    assert_eq!(good.status, 200);
    assert_eq!(
        good.body.unwrap()["result"]["structuredContent"]["store"],
        "headless"
    );
}

fn ask_tool(long_running: bool) -> ToolDef {
    ToolDef {
        name: if long_running {
            "ask_slow".into()
        } else {
            "ask".into()
        },
        description: "asks a question".into(),
        input_schema: json!({"type": "object"}),
        output_schema: None,
        long_running,
        handler: Arc::new(|ctx, _| {
            let a = (ctx.elicit)(
                json!({"mode": "form", "message": "which edge?", "requestedSchema": {"type": "object"}, "elicitationId": "x"}),
            );
            if a["action"] != "accept" {
                return Ok(ToolOutput::error("cancelled"));
            }
            (ctx.progress)(1.0, Some(2.0), Some("halfway".into()));
            Ok(ToolOutput::structured(
                json!({"answer": a["content"]["edge"]}),
            ))
        }),
    }
}

#[test]
fn modern_mrtr_input_required_then_retry() {
    let h = harness();
    h.server.register(ask_tool(false));
    let first = h.send(modern(
        1,
        "tools/call",
        json!({"name": "ask", "arguments": {}}),
    ));
    let r = &first["result"];
    assert_eq!(r["resultType"], "input_required");
    let req = &r["inputRequests"]["input_1"];
    assert_eq!(req["method"], "elicitation/create");
    assert!(req["params"].get("elicitationId").is_none());
    let retry = h.send(modern(
        2,
        "tools/call",
        json!({
            "name": "ask", "arguments": {},
            "inputResponses": {"input_1": {"action": "accept", "content": {"edge": "e7"}}},
        }),
    ));
    assert_eq!(retry["result"]["resultType"], "complete");
    assert_eq!(retry["result"]["structuredContent"]["answer"], "e7");
    assert!(h
        .notes
        .lock()
        .unwrap()
        .iter()
        .any(|n| n["method"] == "notifications/progress"));
}

fn poll_task(h: &Harness, task: &str, want: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let g = h.send(modern(50, "tasks/get", json!({"taskId": task})));
        if g["result"]["status"] == want {
            return g["result"].clone();
        }
        assert!(Instant::now() < deadline, "task never reached {want}: {g}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn modern_tasks_park_update_complete_and_listen() {
    let h = harness();
    h.server.register(ask_tool(true));
    let created = h.send(modern(
        1,
        "tools/call",
        json!({"name": "ask_slow", "arguments": {}}),
    ));
    let r = &created["result"];
    assert_eq!(r["resultType"], "task");
    let task = r["taskId"].as_str().unwrap().to_string();
    // Subscribe to the task's notifications.
    let listen = h.conn.handle(
        modern(
            2,
            "subscriptions/listen",
            json!({"notifications": {"taskIds": [task], "toolsListChanged": true}}),
        ),
        None,
    );
    assert!(listen.listen.is_some() && listen.body.is_none());
    {
        let notes = h.notes.lock().unwrap();
        let ack = notes
            .iter()
            .find(|n| n["method"] == "notifications/subscriptions/acknowledged")
            .unwrap();
        assert_eq!(ack["params"]["notifications"]["toolsListChanged"], true);
        assert_eq!(ack["params"]["_meta"][META_SUBSCRIPTION_ID], 2);
    }
    let parked = poll_task(&h, &task, "input_required");
    assert!(parked["inputRequests"]["input_1"].is_object());
    h.send(modern(3, "tasks/update", json!({"taskId": task, "inputResponses": {"input_1": {"action": "accept", "content": {"edge": "e2"}}}})));
    let done = poll_task(&h, &task, "completed");
    assert_eq!(done["result"]["structuredContent"]["answer"], "e2");
    assert!(h
        .notes
        .lock()
        .unwrap()
        .iter()
        .any(|n| n["method"] == "notifications/tasks" && n["params"]["taskId"] == task.as_str()));
    // toolsListChanged on register.
    h.server.register(ask_tool(false));
    assert!(h
        .notes
        .lock()
        .unwrap()
        .iter()
        .any(|n| n["method"] == "notifications/tools/list_changed"));
    // Unknown task.
    assert_eq!(
        h.send(modern(4, "tasks/get", json!({"taskId": "task_nope"})))["error"]["code"],
        ERR_INVALID_PARAMS
    );
}

#[test]
fn modern_task_cancel() {
    let h = harness();
    h.server.register(ask_tool(true));
    let created = h.send(modern(
        1,
        "tools/call",
        json!({"name": "ask_slow", "arguments": {}}),
    ));
    let task = created["result"]["taskId"].as_str().unwrap().to_string();
    poll_task(&h, &task, "input_required");
    h.send(modern(2, "tasks/cancel", json!({"taskId": task})));
    let c = poll_task(&h, &task, "cancelled");
    assert_eq!(c["statusMessage"], "cancelled by client");
}

#[test]
fn modern_resources_and_not_found_renumbered() {
    let h = harness();
    let out = h.call("create_cad_loon", json!({"source": "[cube 2 2 2]"}));
    let id = out["structuredContent"]["document_id"]
        .as_str()
        .unwrap()
        .to_string();
    let read = h.send(modern(
        1,
        "resources/read",
        json!({"uri": "vcad://doc/current"}),
    ));
    assert_eq!(
        read["result"]["contents"][0]["_meta"]["io.vcad/documentId"],
        id.as_str()
    );
    let list = h.send(modern(2, "resources/list", json!({})));
    assert!(list["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["uri"] == format!("vcad://doc/{id}")));
    let missing = h.send(modern(
        3,
        "resources/read",
        json!({"uri": "vcad://doc/doc_1_nothere"}),
    ));
    assert_eq!(
        missing["error"]["code"], ERR_INVALID_PARAMS,
        "legacy -32002 renumbered"
    );
}

// ── Core tools on the headless store ──────────────────────────────────

#[test]
fn core_tools_author_inspect_render_export() {
    let h = harness();
    let created = h.call("create_cad_loon", json!({"source": "[cube 10 20 30]"}));
    assert_eq!(created["_meta"]["io.vcad/servedBy"], "native");
    let id = created["structuredContent"]["document_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(id.starts_with("doc_"));

    let ins = h.call("inspect_cad", json!({"document_id": id}));
    let s = &ins["structuredContent"];
    assert!((s["volume"].as_f64().unwrap() - 6000.0).abs() < 1e-3, "{s}");
    assert!((s["surface_area"].as_f64().unwrap() - 2200.0).abs() < 1e-3);
    assert!((s["bounding_box"]["max"]["z"].as_f64().unwrap() - 30.0).abs() < 1e-6);
    assert!((s["center_of_mass"]["y"].as_f64().unwrap() - 10.0).abs() < 1e-6);

    let png = h.call(
        "render_view",
        json!({"document_id": id, "view": "top", "width_px": 128}),
    );
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png["content"][0]["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(&bytes[..4], b"\x89PNG");

    let out = tempfile::tempdir().unwrap();
    let stl = out.path().join("part.stl");
    let ex = h.call(
        "export_cad",
        json!({"document_id": id, "filename": stl.to_str().unwrap()}),
    );
    let tris = ins["structuredContent"]["triangles"].as_u64().unwrap();
    assert_eq!(ex["structuredContent"]["bytes"], 84 + 50 * tris);
    assert_eq!(std::fs::metadata(&stl).unwrap().len(), 84 + 50 * tris);

    // Replace the session's content in place.
    h.call(
        "create_cad_loon",
        json!({"document_id": id, "source": "[cube 1 1 1]"}),
    );
    let v = h.call("inspect_cad", json!({"document_id": id}))["structuredContent"]["volume"]
        .as_f64()
        .unwrap();
    assert!((v - 1.0).abs() < 1e-6);

    let doc = h.call("get_document", json!({"document_id": id}));
    let parsed: Value = serde_json::from_str(doc["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(parsed["nodes"].is_object() && parsed["roots"].is_array());

    assert_eq!(
        h.call("close_document", json!({"document_id": id}))["structuredContent"]["closed"],
        true
    );
    assert_eq!(
        h.call("close_document", json!({"document_id": id}))["structuredContent"]["closed"],
        false
    );
    let gone = h.call("inspect_cad", json!({"document_id": id}));
    assert_eq!(gone["isError"], true);

    let bad = h.call("create_cad_loon", json!({"source": "[cube"}));
    assert_eq!(bad["isError"], true);

    let info = h.call("server_info", json!({}));
    assert_eq!(info["structuredContent"]["durable"], true);
    assert_eq!(
        info["structuredContent"]["boot_token"],
        vcad_mcp::store::boot_token()
    );
}

#[test]
fn headless_store_is_node_compatible() {
    let h = harness();
    let dir = h.server.store().location().unwrap();
    // A file the node server wrote, using the legacy `{ ir }` envelope.
    let doc = vcad_loon::eval_vcad("[cube 3 3 3]", None).unwrap();
    std::fs::write(
        dir.join("doc_7_nodeAAAAxyz.json"),
        json!({"ir": doc}).to_string(),
    )
    .unwrap();
    let v = h.call("inspect_cad", json!({"document_id": "doc_7_nodeAAAAxyz"}))["structuredContent"]
        ["volume"]
        .as_f64()
        .unwrap();
    assert!((v - 27.0).abs() < 1e-6);
    // And what we write is a raw Document node can read.
    let id = h.call("open_document", json!({}))["structuredContent"]["document_id"]
        .as_str()
        .unwrap()
        .to_string();
    let raw: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{id}.json"))).unwrap())
            .unwrap();
    assert!(raw.get("nodes").is_some() && raw.get("roots").is_some());
    // A restart-orphaned id reports SESSION LOST, a typo reports unknown.
    let lost = h.call("get_document", json!({"document_id": "doc_3_ZZZZqqqqqq"}));
    assert!(
        lost["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("SESSION LOST"),
        "{lost}"
    );
    let typo = h.call("get_document", json!({"document_id": "nope"}));
    assert!(typo["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Unknown document_id"));
    // Path escape is refused.
    assert_eq!(
        h.call("get_document", json!({"document_id": "../x"}))["isError"],
        true
    );

    let src = tempfile::tempdir().unwrap();
    let f = src.path().join("bolt.loon");
    std::fs::write(&f, "[cylinder 2 10]").unwrap();
    let loaded = h.call("load_document", json!({"path": f.to_str().unwrap()}));
    assert_eq!(loaded["structuredContent"]["roots"], 1);
}

// ── Proxy round trip ──────────────────────────────────────────────────

#[test]
fn proxy_round_trip_to_upstream() {
    let session_dir = tempfile::tempdir().unwrap();
    let proxy = ProxyConfig {
        program: env!("CARGO_BIN_EXE_vcad-mcp").into(),
        args: vec![
            "--headless".into(),
            "--no-proxy".into(),
            "--session-dir".into(),
            session_dir.path().display().to_string(),
        ],
        env: vec![("VCAD_MCP_TEST_UPSTREAM".into(), "1".into())],
    };
    let h = harness_with(Some(proxy));
    let tl = h.send(modern(1, "tools/list", json!({})));
    let names: Vec<&str> = tl["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"upstream_echo"), "{names:?}");
    assert_eq!(
        names.iter().filter(|n| **n == "inspect_cad").count(),
        1,
        "native tools shadow upstream duplicates"
    );
    let r = h.call("upstream_echo", json!({"x": 42}));
    assert_eq!(r["structuredContent"]["echo"]["x"], 42);
    assert_eq!(r["structuredContent"]["served_by"], "upstream");
    assert_eq!(r["_meta"]["io.vcad/servedBy"], "node-proxy");
    // Legacy era through the same proxy.
    h.init_legacy();
    let l = h.send(legacy(
        5,
        "tools/call",
        json!({"name": "upstream_echo", "arguments": {"y": 1}}),
    ));
    assert_eq!(l["result"]["structuredContent"]["echo"]["y"], 1);
}

#[test]
fn unknown_tool_without_proxy() {
    let h = harness();
    let r = h.send(modern(
        1,
        "tools/call",
        json!({"name": "no_such_tool", "arguments": {}}),
    ));
    assert_eq!(r["error"]["code"], ERR_INVALID_PARAMS);
}

// ── Transports ────────────────────────────────────────────────────────

fn http_post(
    addr: std::net::SocketAddr,
    body: &Value,
    headers: &[(&str, &str)],
) -> (u16, String, Value) {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    let payload = body.to_string();
    let mut req = format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n", payload.len());
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(&payload);
    s.write_all(req.as_bytes()).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    let (head, body) = resp.split_once("\r\n\r\n").unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (
        status,
        head.to_string(),
        serde_json::from_str(body).unwrap_or(Value::Null),
    )
}

#[test]
fn http_transport_both_eras() {
    let h = harness();
    let listener = vcad_mcp::transport::bind_http("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = listener.local_addr().unwrap();
    let server = h.server.clone();
    std::thread::spawn(move || vcad_mcp::transport::serve_http(&server, listener));
    assert!(
        vcad_mcp::transport::bind_http("0.0.0.0:0".parse().unwrap()).is_err(),
        "non-loopback refused"
    );

    let (st, head, body) = http_post(
        addr,
        &legacy(
            1,
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {}}),
        ),
        &[],
    );
    assert_eq!(st, 200);
    assert!(head.to_ascii_lowercase().contains("mcp-session-id"));
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");

    let msg = modern(
        2,
        "tools/call",
        json!({"name": "server_info", "arguments": {}}),
    );
    let (st, _, body) = http_post(
        addr,
        &msg,
        &[
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "server_info"),
            ("MCP-Protocol-Version", PROTOCOL_2026),
        ],
    );
    assert_eq!(st, 200, "{body}");
    assert_eq!(body["result"]["resultType"], "complete");
    let (st, _, body) = http_post(addr, &msg, &[]);
    assert_eq!(st, 400);
    assert_eq!(body["error"]["code"], ERR_HEADER_MISMATCH);
    let (st, _, _) = http_post(addr, &msg, &[("Origin", "https://evil.example")]);
    assert_eq!(st, 403);
}

#[test]
fn stdio_lines_transport() {
    let h = harness();
    let input = [
        legacy(
            1,
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {}}),
        )
        .to_string(),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        "not json".to_string(),
        json!({"jsonrpc": "2.0", "id": 2, "method": "server/discover"}).to_string(),
    ]
    .join("\n");
    let out = Arc::new(Mutex::new(Vec::<u8>::new()));
    struct W(Arc<Mutex<Vec<u8>>>);
    impl Write for W {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    vcad_mcp::transport::serve_lines(&h.server, input.as_bytes(), W(out.clone()));
    let text = String::from_utf8(out.lock().unwrap().clone()).unwrap();
    let msgs: Vec<Value> = BufReader::new(text.as_bytes())
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect();
    assert!(msgs
        .iter()
        .any(|m| m["id"] == 1 && m["result"]["serverInfo"]["name"] == "vcad"));
    assert!(msgs.iter().any(|m| m["error"]["code"] == ERR_PARSE));
    assert!(msgs
        .iter()
        .any(|m| m["id"] == 2 && m["result"]["resultType"] == "complete"));
    let _ = Ordering::Relaxed;
}

#[cfg(unix)]
#[test]
fn unix_socket_transport() {
    let h = harness();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vcad.sock");
    let (server, p2) = (h.server.clone(), path.clone());
    std::thread::spawn(move || vcad_mcp::transport::run_unix(&server, &p2));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut s = loop {
        if let Ok(s) = std::os::unix::net::UnixStream::connect(&path) {
            break s;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    };
    writeln!(
        s,
        "{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover"})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(s.try_clone().unwrap())
        .read_line(&mut line)
        .unwrap();
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["result"]["supportedVersions"][0], PROTOCOL_2026);
}

/// A long-running tool that reports progress until cancelled.
fn spin_tool() -> ToolDef {
    ToolDef {
        name: "spin".into(),
        description: "runs until cancelled".into(),
        input_schema: json!({"type": "object"}),
        output_schema: None,
        long_running: true,
        handler: Arc::new(|ctx, _| {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut i = 0.0;
            while !ctx.cancel.load(Ordering::Relaxed) && Instant::now() < deadline {
                (ctx.progress)(i, Some(100.0), Some("spinning".into()));
                i = (i + 1.0f64).min(99.0);
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(ToolOutput::error("spin cancelled"))
        }),
    }
}

fn job_events(h: &Harness) -> Arc<Mutex<Vec<Value>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    h.server.bus().on(Arc::new(move |e| {
        if let vcad_mcp::tasks::BusEvent::Job(v) = e {
            s2.lock().unwrap().push(v.clone());
        }
    }));
    seen
}

fn wait_for(seen: &Arc<Mutex<Vec<Value>>>, f: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(v) = seen.lock().unwrap().iter().find(|v| f(v)) {
            return v.clone();
        }
        assert!(Instant::now() < deadline, "job event never arrived");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_task_is_a_job_and_the_app_cancel_cancels_the_task() {
    let h = harness();
    h.server.register(spin_tool());
    let seen = job_events(&h);
    let created = h.send(modern(
        1,
        "tools/call",
        json!({"name": "spin", "arguments": {}}),
    ));
    let task = created["result"]["taskId"].as_str().unwrap().to_string();
    let running = wait_for(&seen, |v| {
        v["state"] == "running" && v.get("fraction").is_some()
    });
    assert_eq!(running["jobId"], task.as_str());
    assert_eq!(running["taskId"], task.as_str());
    assert_eq!(running["stage"], "spinning");
    let listed = h.send(modern(2, "vcad/jobs/list", json!({})));
    assert_eq!(listed["result"]["jobs"][0]["tool"], "spin");
    // Progress the app reports for work it runs under the job reaches the
    // agent's Task status too.
    h.send(modern(
        9,
        "vcad/jobs/progress",
        json!({"jobId": task, "fraction": 0.5, "stage": "Meshing level 2 of 3"}),
    ));
    wait_for(&seen, |v| {
        v["stage"] == "Meshing level 2 of 3" && v["fraction"] == 0.5
    });
    // The app's ✕.
    let c = h.send(modern(3, "vcad/jobs/cancel", json!({"jobId": task})));
    assert_eq!(c["result"]["cancelled"], task.as_str());
    let got = poll_task(&h, &task, "cancelled");
    assert_eq!(got["status"], "cancelled");
    wait_for(&seen, |v| {
        v["state"] == "cancelled" && v["jobId"] == task.as_str()
    });
}

#[test]
fn a_synchronous_long_call_is_a_job_and_tasks_cancel_ends_it() {
    let h = harness();
    h.server.register(spin_tool());
    let seen = job_events(&h);
    h.init_legacy();
    // A client that did NOT ask for a Task still shows up as a job.
    let conn = h.conn.clone();
    let call = std::thread::spawn(move || {
        conn.handle(
            legacy(1, "tools/call", json!({"name": "spin", "arguments": {}})),
            None,
        )
    });
    let running = wait_for(&seen, |v| v["state"] == "running");
    let id = running["jobId"].as_str().unwrap().to_string();
    assert!(running.get("taskId").is_none());
    h.send(legacy(2, "vcad/jobs/cancel", json!({"jobId": id})));
    let out = call.join().unwrap().body.unwrap();
    assert_eq!(out["result"]["isError"], true);
    wait_for(&seen, |v| {
        v["state"] == "cancelled" && v["jobId"] == id.as_str()
    });
    let missing = h.send(legacy(3, "vcad/jobs/cancel", json!({"jobId": id})));
    assert!(missing["error"]["message"]
        .as_str()
        .unwrap()
        .contains("No running job"));

    // tasks/cancel from the agent ends the job the same way.
    let created = h.send(modern(
        4,
        "tools/call",
        json!({"name": "spin", "arguments": {}}),
    ));
    let task = created["result"]["taskId"].as_str().unwrap().to_string();
    wait_for(&seen, |v| {
        v["state"] == "running" && v["jobId"] == task.as_str()
    });
    h.send(modern(5, "tasks/cancel", json!({"taskId": task})));
    wait_for(&seen, |v| {
        v["state"] == "cancelled" && v["jobId"] == task.as_str()
    });
}
