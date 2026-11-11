//! Strangler proxy: tools without a native port are forwarded to the node
//! server (`packages/mcp`), spawned as a child speaking legacy MCP on stdio.
//! Node retires when this forwards nothing.
//!
//! The child shares the session directory (`VCAD_MCP_SESSION_DIR`), so
//! documents created natively are visible to node tools and vice versa (node
//! hydrates from disk on a cache miss).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::protocol::{ERR_INTERNAL, LATEST_LEGACY};
use crate::tools::{ElicitFn, ProgressFn};

/// How to launch the upstream.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Program.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Extra environment.
    pub env: Vec<(String, String)>,
}

impl ProxyConfig {
    /// `VCAD_MCP_NODE_CMD` (whitespace-split) or `npx -y @vcad/mcp`.
    /// `VCAD_MCP_NODE_CMD=off` disables the proxy.
    pub fn from_env() -> Option<Self> {
        let cmd = std::env::var("VCAD_MCP_NODE_CMD").unwrap_or_else(|_| "npx -y @vcad/mcp".into());
        if matches!(cmd.trim(), "" | "0" | "off" | "none") {
            return None;
        }
        let mut parts = cmd.split_whitespace().map(str::to_string);
        Some(ProxyConfig {
            program: parts.next()?,
            args: parts.collect(),
            env: Vec::new(),
        })
    }
}

/// A JSON-RPC error from the upstream.
#[derive(Debug, Clone)]
pub struct RpcError {
    /// Code.
    pub code: i64,
    /// Message.
    pub message: String,
    /// Data.
    pub data: Option<Value>,
}

impl RpcError {
    fn internal(m: impl Into<String>) -> Self {
        RpcError {
            code: ERR_INTERNAL,
            message: m.into(),
            data: None,
        }
    }
}

/// In-flight upstream requests awaiting a response, by JSON-RPC id.
type PendingMap = Arc<Mutex<HashMap<u64, Sender<Result<Value, RpcError>>>>>;

struct Call {
    progress: Option<ProgressFn>,
    elicit: Option<ElicitFn>,
}

struct Live {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: PendingMap,
    calls: Arc<Mutex<Vec<(u64, Call)>>>,
    dead: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The node upstream.
pub struct Proxy {
    config: ProxyConfig,
    live: Mutex<Option<Arc<Live>>>,
    next_id: AtomicU64,
    tools: Mutex<Option<Vec<Value>>>,
    on_tools_changed: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Upstream spawn failed: don't retry on every list.
    failed: std::sync::atomic::AtomicBool,
}

fn write_line(stdin: &Mutex<ChildStdin>, v: &Value) -> std::io::Result<()> {
    let mut s = stdin.lock().unwrap_or_else(|e| e.into_inner());
    let mut line = v.to_string();
    line.push('\n');
    s.write_all(line.as_bytes())?;
    s.flush()
}

impl Proxy {
    /// A proxy that spawns lazily on first use.
    pub fn new(config: ProxyConfig) -> Self {
        Proxy {
            config,
            live: Mutex::new(None),
            next_id: AtomicU64::new(1),
            tools: Mutex::new(None),
            on_tools_changed: Mutex::new(None),
            failed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Called when the upstream reports `notifications/tools/list_changed`.
    pub fn set_on_tools_changed(&self, f: Arc<dyn Fn() + Send + Sync>) {
        *self
            .on_tools_changed
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    fn spawn(&self) -> Result<Arc<Live>, RpcError> {
        let mut cmd = Command::new(&self.config.program);
        cmd.args(&self.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (k, v) in &self.config.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| {
            RpcError::internal(format!(
                "node upstream `{}` failed to start: {e}",
                self.config.program
            ))
        })?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().expect("piped")));
        let stdout = child.stdout.take().expect("piped");
        let pending: PendingMap = Arc::default();
        let calls: Arc<Mutex<Vec<(u64, Call)>>> = Arc::default();
        let dead = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let (pending, calls, dead, stdin) =
                (pending.clone(), calls.clone(), dead.clone(), stdin.clone());
            let changed = self
                .on_tools_changed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    let id = msg.get("id").cloned();
                    let method = msg
                        .get("method")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    match (id, method) {
                        (Some(id), None) => {
                            let Some(n) = id.as_u64() else { continue };
                            if let Some(tx) =
                                pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&n)
                            {
                                let r = if let Some(err) = msg.get("error") {
                                    Err(RpcError {
                                        code: err
                                            .get("code")
                                            .and_then(Value::as_i64)
                                            .unwrap_or(ERR_INTERNAL),
                                        message: err
                                            .get("message")
                                            .and_then(Value::as_str)
                                            .unwrap_or("")
                                            .to_string(),
                                        data: err.get("data").cloned(),
                                    })
                                } else {
                                    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
                                };
                                let _ = tx.send(r);
                            }
                        }
                        (Some(id), Some(m)) => {
                            // Server-initiated request. Elicitation goes to the
                            // newest in-flight call that can answer it; the
                            // wire carries no call correlation.
                            let elicit = if m == "elicitation/create" {
                                calls
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .iter()
                                    .rev()
                                    .find_map(|(_, c)| c.elicit.clone())
                            } else {
                                None
                            };
                            let stdin = stdin.clone();
                            let params = msg.get("params").cloned().unwrap_or(Value::Null);
                            std::thread::spawn(move || {
                                let reply = match (m.as_str(), elicit) {
                                    ("ping", _) => json!({"jsonrpc":"2.0","id":id,"result":{}}),
                                    ("elicitation/create", Some(f)) => {
                                        json!({"jsonrpc":"2.0","id":id,"result": f(params)})
                                    }
                                    ("elicitation/create", None) => {
                                        json!({"jsonrpc":"2.0","id":id,"result":{"action":"cancel"}})
                                    }
                                    _ => {
                                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"unsupported"}})
                                    }
                                };
                                let _ = write_line(&stdin, &reply);
                            });
                        }
                        (None, Some(m)) => {
                            if m == "notifications/progress" {
                                let p = msg.get("params").cloned().unwrap_or(Value::Null);
                                let token = p.get("progressToken").and_then(Value::as_u64);
                                let cb = calls
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .iter()
                                    .find(|(id, _)| Some(*id) == token)
                                    .and_then(|(_, c)| c.progress.clone());
                                if let Some(cb) = cb {
                                    cb(
                                        p.get("progress").and_then(Value::as_f64).unwrap_or(0.0),
                                        p.get("total").and_then(Value::as_f64),
                                        p.get("message")
                                            .and_then(Value::as_str)
                                            .map(str::to_string),
                                    );
                                }
                            } else if m == "notifications/tools/list_changed" {
                                if let Some(f) = &changed {
                                    f();
                                }
                            }
                        }
                        _ => {}
                    }
                }
                dead.store(true, Ordering::Relaxed);
                for (_, tx) in pending.lock().unwrap_or_else(|e| e.into_inner()).drain() {
                    let _ = tx.send(Err(RpcError::internal("node upstream exited")));
                }
            });
        }
        let live = Arc::new(Live {
            child,
            stdin,
            pending,
            calls,
            dead,
        });
        // Legacy handshake.
        self.request_on(
            &live,
            "initialize",
            json!({
                "protocolVersion": LATEST_LEGACY,
                "capabilities": { "elicitation": {} },
                "clientInfo": { "name": "vcad-mcp-proxy", "version": env!("CARGO_PKG_VERSION") },
            }),
            Duration::from_secs(300),
        )?;
        write_line(
            &live.stdin,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .map_err(|e| RpcError::internal(e.to_string()))?;
        Ok(live)
    }

    fn live(&self) -> Result<Arc<Live>, RpcError> {
        let mut guard = self.live.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(l) = guard.as_ref() {
            if !l.dead.load(Ordering::Relaxed) {
                return Ok(l.clone());
            }
        }
        let l = self.spawn()?;
        *guard = Some(l.clone());
        Ok(l)
    }

    fn request_on(
        &self,
        live: &Live,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.request_with_id(live, id, method, params, timeout)
    }

    fn request_with_id(
        &self,
        live: &Live,
        id: u64,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        let (tx, rx) = channel();
        live.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        write_line(
            &live.stdin,
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
        )
        .map_err(|e| RpcError::internal(format!("node upstream write failed: {e}")))?;
        rx.recv_timeout(timeout).unwrap_or_else(|_| {
            Err(RpcError::internal(format!(
                "node upstream timed out on {method}"
            )))
        })
    }

    /// Upstream tool listings (cached; `None` when the upstream is unavailable).
    pub fn list_tools(&self) -> Option<Vec<Value>> {
        if let Some(t) = self.tools.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Some(t);
        }
        if self.failed.load(Ordering::Relaxed) {
            return None;
        }
        let live = match self.live() {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[vcad-mcp] proxy disabled: {}", e.message);
                self.failed.store(true, Ordering::Relaxed);
                return None;
            }
        };
        let mut out = Vec::new();
        let mut cursor: Option<Value> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let r = self
                .request_on(&live, "tools/list", params, Duration::from_secs(120))
                .ok()?;
            if let Some(ts) = r.get("tools").and_then(Value::as_array) {
                out.extend(ts.iter().cloned());
            }
            cursor = r.get("nextCursor").filter(|c| !c.is_null()).cloned();
            if cursor.is_none() {
                break;
            }
        }
        *self.tools.lock().unwrap_or_else(|e| e.into_inner()) = Some(out.clone());
        Some(out)
    }

    /// Forget the cached tool list.
    pub fn invalidate_tools(&self) {
        *self.tools.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Forward a `tools/call`. Progress routes by our own progressToken.
    pub fn call_tool(
        &self,
        name: &str,
        args: Value,
        progress: Option<ProgressFn>,
        elicit: Option<ElicitFn>,
    ) -> Result<Value, RpcError> {
        let live = self.live()?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        live.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((id, Call { progress, elicit }));
        let params = json!({"name": name, "arguments": args, "_meta": {"progressToken": id}});
        let r = self.request_with_id(
            &live,
            id,
            "tools/call",
            params,
            Duration::from_secs(6 * 3600),
        );
        live.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(i, _)| *i != id);
        r
    }
}
