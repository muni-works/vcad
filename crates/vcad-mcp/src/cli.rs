//! `vcad mcp`: one entry for every way to run the server.
//!
//! Default (stdio): attach to a running vcad.app (per-instance discovery,
//! `~/.vcad/instances`; `VCAD_APP_PID` / `VCAD_APP_URL` / `VCAD_APP_DOCUMENT`
//! choose), else a serving unix socket, else run the same core headless.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::server::{Server, ServerConfig};
use crate::store::HeadlessStore;
use crate::transport;

/// Options for `vcad mcp` (the CLI crate maps its clap args onto this).
#[derive(Debug, Clone, Default)]
pub struct McpOptions {
    /// Serve HTTP on this loopback address instead of stdio.
    pub http: Option<SocketAddr>,
    /// Serve on this unix socket instead of stdio (headless host for the shim).
    pub serve_socket: Option<PathBuf>,
    /// Never attach to a running app; always run the headless core.
    pub headless: bool,
    /// Socket to attach to (default `~/.vcad/vcad.sock`).
    pub socket: Option<PathBuf>,
    /// Disable the node proxy (native tools only).
    pub no_proxy: bool,
    /// Session directory override.
    pub session_dir: Option<PathBuf>,
}

/// Parse `vcad-mcp` / `vcad mcp` style flags (for the standalone binary).
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<McpOptions, String> {
    let mut o = McpOptions::default();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--http" => o.http = Some(val("--http")?.parse().map_err(|e| format!("--http: {e}"))?),
            "--serve-socket" => o.serve_socket = Some(val("--serve-socket")?.into()),
            "--socket" => o.socket = Some(val("--socket")?.into()),
            "--session-dir" => o.session_dir = Some(val("--session-dir")?.into()),
            "--headless" => o.headless = true,
            "--no-proxy" => o.no_proxy = true,
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument '{other}'\n{USAGE}")),
        }
    }
    Ok(o)
}

/// Usage text.
pub const USAGE: &str = "usage: vcad mcp [--headless] [--socket PATH] [--http 127.0.0.1:PORT] [--serve-socket PATH] [--no-proxy] [--session-dir DIR]";

/// Build the headless server for `opts`.
pub fn headless_server(opts: &McpOptions) -> Server {
    let store = match &opts.session_dir {
        Some(d) => HeadlessStore::new(d),
        None => HeadlessStore::from_env(),
    };
    let mut config = ServerConfig::default();
    if opts.no_proxy {
        config.proxy = None;
    }
    Server::new(Arc::new(store), config)
}

/// Run until the transport ends.
pub fn run(opts: McpOptions) -> std::io::Result<()> {
    if let Some(addr) = opts.http {
        let server = headless_server(&opts);
        eprintln!("[vcad-mcp] HTTP on http://{addr}/mcp");
        return transport::run_http(&server, addr);
    }
    #[cfg(unix)]
    if let Some(path) = &opts.serve_socket {
        let server = headless_server(&opts);
        eprintln!("[vcad-mcp] unix socket {}", path.display());
        return transport::run_unix(&server, path);
    }
    if !opts.headless && opts.socket.is_none() {
        // A running vcad.app is the server: same core, the live document.
        let explicit = ["VCAD_APP_URL", "VCAD_APP_PID"]
            .iter()
            .any(|k| std::env::var(k).is_ok_and(|v| !v.is_empty()));
        match crate::discovery::select_from_env() {
            Ok(sel) if crate::discovery::reachable(&sel.url) => {
                if let Some(w) = &sel.warning {
                    eprintln!("[vcad-mcp] {w}");
                }
                eprintln!(
                    "[vcad-mcp] attached to vcad.app {} at {}",
                    sel.instance
                        .as_ref()
                        .map(|r| r.describe())
                        .unwrap_or_default(),
                    sel.url
                );
                return crate::discovery::attach_http_stdio(&sel.url);
            }
            // An explicitly chosen app that is gone is an error, never a
            // silent fall back to headless.
            Ok(sel) if explicit => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    format!("vcad.app at {} is not answering", sel.url),
                ))
            }
            Err(e) if explicit => {
                return Err(std::io::Error::new(std::io::ErrorKind::NotFound, e))
            }
            _ => {}
        }
    }
    #[cfg(unix)]
    if !opts.headless {
        let path = opts
            .socket
            .clone()
            .unwrap_or_else(transport::default_socket_path);
        if path.exists() && transport::attach_stdio(&path).is_ok() {
            return Ok(());
        }
    }
    transport::run_stdio(&headless_server(&opts));
    Ok(())
}
