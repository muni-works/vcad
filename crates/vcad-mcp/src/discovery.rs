//! Finding a running vcad.app (per-instance discovery, #1133) and attaching
//! stdio to it — `vcad mcp`'s default when the app is open.
//!
//! Each app instance writes `~/.vcad/instances/<pid>.json` (`pid`, `port`,
//! `url`, `instance_id`, `started`, `document_name`, `document_path`).
//! Selection mirrors `packages/mcp/src/app-discovery.ts`: `VCAD_APP_URL` →
//! `VCAD_APP_PID` (an error if that pid is gone — never a silent fallback) →
//! the instance whose document matches `VCAD_APP_DOCUMENT` → the newest, with a
//! warning naming the others.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

/// One `instances/<pid>.json`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct InstanceRecord {
    /// Process id.
    pub pid: i64,
    /// Loopback port.
    pub port: u16,
    /// `http://127.0.0.1:<port>/mcp`.
    #[serde(default)]
    pub url: Option<String>,
    /// Random per launch.
    #[serde(default)]
    pub instance_id: Option<String>,
    /// ISO-8601 launch time (sorts lexically).
    #[serde(default)]
    pub started: Option<String>,
    /// Open document's name.
    #[serde(default)]
    pub document_name: Option<String>,
    /// Open document's path.
    #[serde(default)]
    pub document_path: Option<String>,
}

impl InstanceRecord {
    /// The MCP endpoint.
    pub fn url(&self) -> String {
        self.url
            .clone()
            .unwrap_or_else(|| format!("http://127.0.0.1:{}/mcp", self.port))
    }

    /// "pid 123 (bracket.vcad)".
    pub fn describe(&self) -> String {
        let doc = self
            .document_name
            .clone()
            .or_else(|| self.document_path.clone())
            .unwrap_or_else(|| "no document".into());
        format!("pid {} ({doc})", self.pid)
    }
}

/// The chosen target.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    /// Endpoint URL.
    pub url: String,
    /// The record, when the URL came from one.
    pub instance: Option<InstanceRecord>,
    /// Several instances and none chosen explicitly.
    pub warning: Option<String>,
}

/// `~/.vcad/instances`.
pub fn instances_dir() -> PathBuf {
    crate::store::vcad_home().join("instances")
}

#[cfg(unix)]
fn pid_alive(pid: i64) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    if pid <= 0 || pid > i32::MAX as i64 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists.
    let r = unsafe { kill(pid as i32, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(1) // EPERM
}

#[cfg(not(unix))]
fn pid_alive(_pid: i64) -> bool {
    true
}

/// Live instances in `dir`, newest first.
pub fn list_instances(dir: &Path) -> Vec<InstanceRecord> {
    let mut out: Vec<InstanceRecord> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .filter(|r: &InstanceRecord| pid_alive(r.pid))
        .collect();
    out.sort_by(|a, b| b.started.cmp(&a.started));
    out
}

fn same_path(a: &str, b: &str) -> bool {
    let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
    canon(a) == canon(b)
}

/// Pick a target from `instances` given environment lookups `env`.
pub fn select(
    instances: &[InstanceRecord],
    env: impl Fn(&str) -> Option<String>,
) -> Result<Selection, String> {
    if let Some(url) = env("VCAD_APP_URL").filter(|u| !u.is_empty()) {
        let instance = instances.iter().find(|r| r.url() == url).cloned();
        return Ok(Selection {
            url,
            instance,
            warning: None,
        });
    }
    let running = || {
        instances
            .iter()
            .map(InstanceRecord::describe)
            .collect::<Vec<_>>()
            .join(", ")
    };
    if let Some(pid) = env("VCAD_APP_PID").filter(|p| !p.is_empty()) {
        let found = pid
            .parse::<i64>()
            .ok()
            .and_then(|p| instances.iter().find(|r| r.pid == p));
        return match found {
            Some(r) => Ok(Selection {
                url: r.url(),
                instance: Some(r.clone()),
                warning: None,
            }),
            None => Err(format!(
                "VCAD_APP_PID={pid}: no running vcad.app with that pid.{}",
                if instances.is_empty() {
                    String::new()
                } else {
                    format!(" Running: {}.", running())
                }
            )),
        };
    }
    let Some(newest) = instances.first() else {
        return Err("no running vcad.app found (~/.vcad/instances is empty)".into());
    };
    if let Some(wanted) = env("VCAD_APP_DOCUMENT").filter(|d| !d.is_empty()) {
        if let Some(r) = instances
            .iter()
            .find(|r| r.document_path.as_deref().is_some_and(|p| same_path(p, &wanted)))
        {
            return Ok(Selection {
                url: r.url(),
                instance: Some(r.clone()),
                warning: None,
            });
        }
    }
    let warning = (instances.len() > 1).then(|| {
        format!(
            "{} vcad.app instances are running; using the newest, {}. Others: {}. Set VCAD_APP_PID to choose.",
            instances.len(),
            newest.describe(),
            instances[1..]
                .iter()
                .map(InstanceRecord::describe)
                .collect::<Vec<_>>()
                .join(", ")
        )
    });
    Ok(Selection {
        url: newest.url(),
        instance: Some(newest.clone()),
        warning,
    })
}

/// Select from the real environment and `~/.vcad/instances`.
pub fn select_from_env() -> Result<Selection, String> {
    select(&list_instances(&instances_dir()), |k| std::env::var(k).ok())
}

fn host_port_path(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("http://")?;
    let (hp, path) = rest.split_once('/').unwrap_or((rest, ""));
    Some((hp.to_string(), format!("/{path}")))
}

/// One JSON-RPC message over HTTP POST; returns the response body (empty for
/// a notification's `202`).
pub fn post(url: &str, body: &str, timeout: Option<Duration>) -> std::io::Result<String> {
    let (hp, path) = host_port_path(url).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("bad url {url}"))
    })?;
    let mut s = TcpStream::connect(&hp)?;
    s.set_read_timeout(timeout)?;
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: {hp}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMcp-Session-Id: vcad-cli\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw);
    let body = text.split_once("\r\n\r\n").map(|x| x.1).unwrap_or("");
    Ok(body.trim().to_string())
}

/// Whether something answers at `url` (a `ping`).
pub fn reachable(url: &str) -> bool {
    post(
        url,
        r#"{"jsonrpc":"2.0","id":"vcad-cli-probe","method":"ping"}"#,
        Some(Duration::from_secs(3)),
    )
    .is_ok_and(|b| b.contains("vcad-cli-probe"))
}

/// Pipe stdio (newline-delimited JSON-RPC) to the app's HTTP endpoint, and
/// its server→client notifications (`GET` event stream) back to stdout.
pub fn attach_http_stdio(url: &str) -> std::io::Result<()> {
    let out = std::sync::Arc::new(std::sync::Mutex::new(std::io::stdout()));
    let emit = {
        let out = out.clone();
        move |line: &str| {
            let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
            let _ = writeln!(o, "{line}");
            let _ = o.flush();
        }
    };
    // Notifications stream (resource updates, tool list changes).
    if let Some((hp, path)) = host_port_path(url) {
        let emit = emit.clone();
        std::thread::spawn(move || {
            let Ok(mut s) = TcpStream::connect(&hp) else {
                return;
            };
            let _ = write!(
                s,
                "GET {path} HTTP/1.1\r\nHost: {hp}\r\nAccept: text/event-stream\r\nMcp-Session-Id: vcad-cli\r\n\r\n"
            );
            for line in BufReader::new(s).lines().map_while(Result::ok) {
                if let Some(data) = line.strip_prefix("data: ") {
                    if serde_json::from_str::<Value>(data).is_ok() {
                        emit(data);
                    }
                }
            }
        });
    }
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let is_request = serde_json::from_str::<Value>(line)
            .ok()
            .is_some_and(|v| v.get("id").is_some() && v.get("method").is_some());
        match post(url, line, None) {
            Ok(body) if is_request && !body.is_empty() => emit(&body),
            Ok(_) => {}
            Err(e) if is_request => {
                let id = serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|v| v.get("id").cloned())
                    .unwrap_or(Value::Null);
                emit(
                    &serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": format!("vcad.app at {url} went away: {e}")}})
                        .to_string(),
                );
            }
            Err(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(pid: i64, started: &str, doc: Option<&str>) -> InstanceRecord {
        InstanceRecord {
            pid,
            port: 47000 + pid as u16,
            url: None,
            instance_id: None,
            started: Some(started.into()),
            document_name: doc.map(|d| d.rsplit('/').next().unwrap().to_string()),
            document_path: doc.map(str::to_string),
        }
    }

    #[test]
    fn selection_rules() {
        let a = rec(1, "2026-11-11T10:00:00Z", Some("/tmp/a.vcad"));
        let b = rec(2, "2026-11-11T09:00:00Z", Some("/tmp/b.vcad"));
        let all = vec![a.clone(), b.clone()];
        let none = |_: &str| None;
        let s = select(&all, none).unwrap();
        assert_eq!(s.instance, Some(a.clone()));
        assert!(s.warning.unwrap().contains("Set VCAD_APP_PID"));
        let s = select(&all, |k| (k == "VCAD_APP_PID").then(|| "2".into())).unwrap();
        assert_eq!(s.instance, Some(b.clone()));
        assert!(select(&all, |k| (k == "VCAD_APP_PID").then(|| "9".into()))
            .unwrap_err()
            .contains("no running vcad.app with that pid"));
        let s = select(&all, |k| (k == "VCAD_APP_DOCUMENT").then(|| "/tmp/b.vcad".into())).unwrap();
        assert_eq!(s.instance, Some(b));
        let s = select(&all, |k| (k == "VCAD_APP_URL").then(|| "http://127.0.0.1:1/mcp".into())).unwrap();
        assert_eq!(s.url, "http://127.0.0.1:1/mcp");
        assert!(select(&[], none).is_err());
        assert!(select(&[a], none).unwrap().warning.is_none());
    }

    #[test]
    fn lists_only_live_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let me = std::process::id() as i64;
        let w = |name: &str, v: Value| std::fs::write(dir.path().join(name), v.to_string()).unwrap();
        w("a.json", serde_json::json!({"pid": me, "port": 1, "started": "2026-01-01"}));
        w("b.json", serde_json::json!({"pid": me, "port": 2, "started": "2026-02-01"}));
        w("dead.json", serde_json::json!({"pid": 999_999_999, "port": 3, "started": "2027-01-01"}));
        let l = list_instances(dir.path());
        assert_eq!(l.iter().map(|r| r.port).collect::<Vec<_>>(), vec![2, 1]);
    }
}
