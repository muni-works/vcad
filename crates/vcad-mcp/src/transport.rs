//! Transports: newline-delimited JSON-RPC over stdio or a unix socket, and
//! Streamable-HTTP-shaped JSON over `127.0.0.1`.
//!
//! Each inbound request is served on its own thread, so a long tool call
//! never blocks a ping, a `tasks/get`, or the client's answer to an
//! elicitation. Responses may therefore arrive out of order (JSON-RPC allows
//! it; ids correlate).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::protocol::{error_response, Headers, ERR_PARSE};
use crate::server::{Connection, Server, Sink};

/// Serve one line-delimited stream until EOF.
pub fn serve_lines<R: Read, W: Write + Send + 'static>(server: &Server, reader: R, writer: W) {
    let writer = Arc::new(Mutex::new(writer));
    let w2 = writer.clone();
    let sink: Sink = Arc::new(move |msg: Value| {
        let mut w = w2.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(w, "{msg}");
        let _ = w.flush();
    });
    let conn = Connection::new(server.clone(), sink.clone());
    let mut workers = Vec::new();
    for line in BufReader::new(reader).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                sink(error_response(
                    &Value::Null,
                    ERR_PARSE,
                    &format!("Parse error: {e}"),
                    None,
                ));
                continue;
            }
        };
        let (conn, sink) = (conn.clone(), sink.clone());
        workers.push(std::thread::spawn(move || {
            let out = conn.handle(msg, None);
            if let Some(body) = out.body {
                sink(body);
            }
        }));
        workers.retain(|w| !w.is_finished());
    }
    // EOF: let in-flight calls finish writing.
    for w in workers {
        let _ = w.join();
    }
}

/// Serve MCP on this process's stdin/stdout.
pub fn run_stdio(server: &Server) {
    serve_lines(server, std::io::stdin(), std::io::stdout());
}

/// Default app socket: `$VCAD_MCP_SOCKET` or `~/.vcad/vcad.sock`.
pub fn default_socket_path() -> PathBuf {
    std::env::var_os("VCAD_MCP_SOCKET")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::store::vcad_home().join("vcad.sock"))
}

/// Serve on a unix socket (what vcad.app runs; `vcad mcp` attaches to it).
#[cfg(unix)]
pub fn run_unix(server: &Server, path: &Path) -> std::io::Result<()> {
    use std::os::unix::net::UnixListener;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A stale socket from a crashed process: only remove it if nobody answers.
    if path.exists() && std::os::unix::net::UnixStream::connect(path).is_err() {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let server = server.clone();
        std::thread::spawn(move || {
            let Ok(w) = stream.try_clone() else { return };
            serve_lines(&server, stream, w);
        });
    }
    Ok(())
}

/// Pipe stdio to a running server's unix socket (the `vcad mcp` shim).
/// Returns `Err` if nothing is listening, so the caller can fall back.
#[cfg(unix)]
pub fn attach_stdio(path: &Path) -> std::io::Result<()> {
    let stream = std::os::unix::net::UnixStream::connect(path)?;
    let mut up = stream.try_clone()?;
    let mut down = stream;
    let t = std::thread::spawn(move || {
        let _ = std::io::copy(&mut down, &mut std::io::stdout().lock());
    });
    let _ = std::io::copy(&mut std::io::stdin().lock(), &mut up);
    let _ = up.shutdown(std::net::Shutdown::Write);
    let _ = t.join();
    Ok(())
}

// ── HTTP (127.0.0.1 only) ────────────────────────────────────────────

/// Serve HTTP on a loopback address. Non-loopback binds are refused: vcad
/// operates no network services.
pub fn run_http(server: &Server, addr: SocketAddr) -> std::io::Result<()> {
    let listener = bind_http(addr)?;
    serve_http(server, listener);
    Ok(())
}

/// Bind (loopback only).
pub fn bind_http(addr: SocketAddr) -> std::io::Result<TcpListener> {
    if !addr.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "vcad-mcp HTTP binds 127.0.0.1 / ::1 only",
        ));
    }
    TcpListener::bind(addr)
}

/// Accept loop over a bound listener.
pub fn serve_http(server: &Server, listener: TcpListener) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let server = server.clone();
        std::thread::spawn(move || {
            let _ = handle_http(&server, stream);
        });
    }
}

struct Request {
    method: String,
    path: String,
    headers: Headers,
    body: Vec<u8>,
}

fn read_request(stream: &mut BufReader<TcpStream>) -> std::io::Result<Option<Request>> {
    let mut line = String::new();
    if stream.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if stream.read_line(&mut h)? == 0 {
            break;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        if headers.len() > 100 {
            return Ok(None);
        }
    }
    let headers = Headers(headers);
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if len > 64 * 1024 * 1024 {
        return Ok(None);
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    Ok(Some(Request {
        method,
        path,
        headers,
        body,
    }))
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    extra: &[(&str, String)],
    body: Option<&Value>,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        payload.len()
    );
    if body.is_some() {
        head.push_str("Content-Type: application/json\r\n");
    }
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(payload.as_bytes())?;
    stream.flush()
}

/// DNS-rebinding guard: a browser page may only reach us from localhost.
fn origin_ok(headers: &Headers) -> bool {
    match headers.get("origin") {
        None => true,
        Some(o) => {
            let host = o.split("://").nth(1).unwrap_or(o);
            let host = host.split(['/', ':']).next().unwrap_or("");
            matches!(host, "localhost" | "127.0.0.1" | "[::1]")
        }
    }
}

/// Streamable-HTTP `GET /mcp`: a server→client event stream of
/// `notifications/resources/updated` and `notifications/tools/list_changed`
/// for clients on the legacy era (vcad.app's agents, `vcad mcp`'s attach).
fn serve_event_stream(server: &Server, mut out: TcpStream) -> std::io::Result<()> {
    out.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nMcp-Session-Id: vcad\r\n\r\n: vcad\n\n")?;
    out.flush()?;
    let w = Arc::new(Mutex::new(out));
    let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (w2, alive2) = (w.clone(), alive.clone());
    let id = server.bus().on(Arc::new(move |evt: &crate::tasks::BusEvent| {
        let msg = match evt {
            crate::tasks::BusEvent::ResourceUpdated(uri) => json!({
                "jsonrpc": "2.0", "method": "notifications/resources/updated", "params": {"uri": uri}
            }),
            crate::tasks::BusEvent::ToolsListChanged => {
                json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
            }
            crate::tasks::BusEvent::Task(_) | crate::tasks::BusEvent::Job(_) => return,
        };
        let mut w = w2.lock().unwrap_or_else(|e| e.into_inner());
        if w.write_all(format!("event: message\ndata: {msg}\n\n").as_bytes())
            .and_then(|_| w.flush())
            .is_err()
        {
            alive2.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }));
    while alive.load(std::sync::atomic::Ordering::Relaxed) {
        std::thread::sleep(Duration::from_secs(15));
        let mut g = w.lock().unwrap_or_else(|e| e.into_inner());
        if g.write_all(b": keepalive\n\n")
            .and_then(|_| g.flush())
            .is_err()
        {
            break;
        }
    }
    server.bus().off(id);
    Ok(())
}

fn handle_http(server: &Server, stream: TcpStream) -> std::io::Result<()> {
    let mut out = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let Some(req) = read_request(&mut reader)? else {
        return Ok(());
    };
    if !origin_ok(&req.headers) {
        return write_response(
            &mut out,
            403,
            &[],
            Some(&json!({"error": "forbidden origin"})),
        );
    }
    if req.path.split('?').next() != Some("/mcp") {
        return write_response(
            &mut out,
            404,
            &[],
            Some(&json!({"error": "not found; POST /mcp"})),
        );
    }
    if req.method == "GET"
        && req
            .headers
            .get("accept")
            .is_some_and(|a| a.contains("text/event-stream"))
    {
        return serve_event_stream(server, out);
    }
    match req.method.as_str() {
        "POST" => {}
        "DELETE" => return write_response(&mut out, 200, &[], None),
        _ => return write_response(&mut out, 405, &[("Allow", "POST, DELETE".into())], None),
    }
    let msg: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(e) => {
            return write_response(
                &mut out,
                400,
                &[],
                Some(&error_response(
                    &Value::Null,
                    ERR_PARSE,
                    &format!("Parse error: {e}"),
                    None,
                )),
            )
        }
    };
    let is_listen = msg.get("method").and_then(Value::as_str) == Some("subscriptions/listen");
    if is_listen {
        // SSE stream: ack + notifications until the client goes away.
        out.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n")?;
        out.flush()?;
        let w = Arc::new(Mutex::new(out.try_clone()?));
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (w2, alive2) = (w.clone(), alive.clone());
        let sink: Sink = Arc::new(move |m: Value| {
            let mut w = w2.lock().unwrap_or_else(|e| e.into_inner());
            if w.write_all(format!("event: message\ndata: {m}\n\n").as_bytes())
                .and_then(|_| w.flush())
                .is_err()
            {
                alive2.store(false, std::sync::atomic::Ordering::Relaxed);
            }
        });
        let outcome = server.handle_modern(&msg, Some(&req.headers), Some(sink));
        let Some(handle) = outcome.listen else {
            // Rejected (version/header): send the error as one event.
            if let Some(b) = outcome.body {
                let _ = w
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .write_all(format!("event: message\ndata: {b}\n\n").as_bytes());
            }
            return Ok(());
        };
        while alive.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(15));
            let mut g = w.lock().unwrap_or_else(|e| e.into_inner());
            if g.write_all(b": keepalive\n\n")
                .and_then(|_| g.flush())
                .is_err()
            {
                break;
            }
        }
        handle.close();
        return Ok(());
    }
    // Request-scoped notifications are dropped on plain-JSON HTTP responses
    // (streaming is an enhancement; the reply is complete without them).
    let conn = Connection::one_shot(server.clone(), Arc::new(|_| {}));
    let outcome = conn.handle(msg, Some(&req.headers));
    let mut extra = Vec::new();
    if let Some(sid) = outcome.session_id {
        extra.push(("Mcp-Session-Id", sid));
    }
    write_response(&mut out, outcome.status, &extra, outcome.body.as_ref())
}
