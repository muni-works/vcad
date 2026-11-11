//! Wire constants and helpers for both MCP eras.
//!
//! Ported from `packages/mcp/src/protocol-2026.ts` (the reference
//! implementation of the `2026-07-28` framing). `rmcp` was evaluated and does
//! not implement this revision (no `server/discover`, MRTR, or
//! `subscriptions/listen`), so the framing is hand-rolled over `serde_json`.

use serde_json::{json, Map, Value};

/// The modern revision.
pub const PROTOCOL_2026: &str = "2026-07-28";

/// Newest legacy (`initialize`-era) revision.
pub const LATEST_LEGACY: &str = "2025-11-25";

/// Every revision served, newest first.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    PROTOCOL_2026,
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];

/// Legacy revisions answerable via `initialize`.
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// `_meta` keys defined by the revision.
pub const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
/// Client capabilities in `_meta`.
pub const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
/// Client identity in `_meta`.
pub const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
/// Server identity stamped on every modern result.
pub const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
/// Opt-in for `notifications/message` on a modern request.
pub const META_LOG_LEVEL: &str = "io.modelcontextprotocol/logLevel";
/// Subscription id on `subscriptions/listen` notifications.
pub const META_SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";
/// vcad build stamp on every tool result (restart detection).
pub const META_VCAD_BUILD: &str = "io.vcad/build";
/// Tasks extension id (SEP-2663).
pub const TASKS_EXTENSION: &str = "io.modelcontextprotocol/tasks";

/// Headers disagree with the body, or a required header is missing.
pub const ERR_HEADER_MISMATCH: i64 = -32020;
/// A required client capability was not declared.
pub const ERR_MISSING_REQUIRED_CLIENT_CAPABILITY: i64 = -32021;
/// Requested protocol version is not served.
pub const ERR_UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
/// JSON-RPC parse error.
pub const ERR_PARSE: i64 = -32700;
/// JSON-RPC invalid request.
pub const ERR_INVALID_REQUEST: i64 = -32600;
/// JSON-RPC method not found.
pub const ERR_METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC invalid params.
pub const ERR_INVALID_PARAMS: i64 = -32602;
/// JSON-RPC internal error.
pub const ERR_INTERNAL: i64 = -32603;
/// Pre-2026 "resource not found", renumbered to invalid params in 2026.
pub const LEGACY_RESOURCE_NOT_FOUND: i64 = -32002;

/// List freshness hint (ms).
pub const LIST_TTL_MS: u64 = 60_000;
/// `server/discover` freshness hint (ms).
pub const DISCOVER_TTL_MS: u64 = 300_000;

/// Methods whose modern results carry `ttlMs` / `cacheScope`.
pub fn cache_ttl(method: &str) -> Option<u64> {
    match method {
        "server/discover" => Some(DISCOVER_TTL_MS),
        "tools/list"
        | "prompts/list"
        | "resources/list"
        | "resources/templates/list"
        | "resources/read" => Some(LIST_TTL_MS),
        _ => None,
    }
}

/// Methods only the modern revision defines.
pub fn is_modern_only_method(method: &str) -> bool {
    matches!(
        method,
        "server/discover" | "subscriptions/listen" | "tasks/get" | "tasks/update" | "tasks/cancel"
    )
}

/// Request methods served in both eras (the "forwarded" set in the TS port).
pub fn is_shared_method(method: &str) -> bool {
    matches!(
        method,
        "tools/list"
            | "tools/call"
            | "vcad/jobs/cancel"
            | "vcad/jobs/progress"
            | "vcad/jobs/list"
            | "resources/list"
            | "resources/templates/list"
            | "resources/read"
            | "prompts/list"
            | "prompts/get"
            | "completion/complete"
    )
}

/// MRTR allowed on these client requests.
pub fn mrtr_allowed(method: &str) -> bool {
    matches!(method, "tools/call" | "prompts/get" | "resources/read")
}

/// Case-insensitive header bag (HTTP only; stdio/unix have none).
#[derive(Debug, Clone, Default)]
pub struct Headers(pub Vec<(String, String)>);

impl Headers {
    /// First value for `name`, case-insensitively.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// `params._meta` of a message, if an object.
pub fn meta_of(msg: &Value) -> Option<&Map<String, Value>> {
    msg.get("params")?.get("_meta")?.as_object()
}

/// True when a message belongs to the modern era: a modern-only method, a
/// body-declared protocol version, or (HTTP) an `Mcp-Method` header. The
/// `MCP-Protocol-Version` header alone is NOT a signal (legacy clients send it).
pub fn is_modern_message(msg: &Value, headers: Option<&Headers>) -> bool {
    let first = match msg {
        Value::Array(a) => match a.first() {
            Some(f) => f,
            None => return false,
        },
        other => other,
    };
    if !first.is_object() {
        return false;
    }
    if let Some(m) = first.get("method").and_then(Value::as_str) {
        if is_modern_only_method(m) {
            return true;
        }
    }
    if meta_of(first)
        .and_then(|m| m.get(META_PROTOCOL_VERSION))
        .is_some_and(Value::is_string)
    {
        return true;
    }
    headers.is_some_and(|h| h.get("mcp-method").is_some())
}

/// Decode the `=?base64?…?=` sentinel used for non-ASCII header values.
pub fn decode_header_value(value: &str) -> String {
    use base64::Engine;
    const PRE: &str = "=?base64?";
    const SUF: &str = "?=";
    if value.len() >= PRE.len() + SUF.len() && value.starts_with(PRE) && value.ends_with(SUF) {
        let enc = &value[PRE.len()..value.len() - SUF.len()];
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(enc) {
            if let Ok(s) = String::from_utf8(bytes) {
                return s;
            }
        }
    }
    value.to_string()
}

/// JSON-RPC error response.
pub fn error_response(id: &Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut err = json!({ "code": code, "message": message });
    if let Some(d) = data {
        err["data"] = d;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": err })
}

/// JSON-RPC success response.
pub fn result_response(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Drop the per-request modern `_meta` keys and MRTR fields from params
/// before handing them to a tool or the legacy upstream.
pub fn strip_modern_fields(params: &Value) -> Value {
    let mut out = params.clone();
    if let Some(obj) = out.as_object_mut() {
        obj.remove("inputResponses");
        obj.remove("requestState");
        let mut drop_meta = false;
        if let Some(meta) = obj.get_mut("_meta").and_then(Value::as_object_mut) {
            for k in [
                META_PROTOCOL_VERSION,
                META_CLIENT_CAPABILITIES,
                META_CLIENT_INFO,
                META_LOG_LEVEL,
            ] {
                meta.remove(k);
            }
            drop_meta = meta.is_empty();
        }
        if drop_meta {
            obj.remove("_meta");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_eras() {
        let legacy = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}});
        assert!(!is_modern_message(&legacy, None));
        let modern = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{META_PROTOCOL_VERSION: PROTOCOL_2026}}});
        assert!(is_modern_message(&modern, None));
        let disc = json!({"jsonrpc":"2.0","id":1,"method":"server/discover"});
        assert!(is_modern_message(&disc, None));
        let h = Headers(vec![("Mcp-Method".into(), "tools/list".into())]);
        assert!(is_modern_message(&json!({"jsonrpc":"2.0"}), Some(&h)));
    }

    #[test]
    fn header_sentinel_roundtrip() {
        assert_eq!(decode_header_value("=?base64?w6l0w6k=?="), "été");
        assert_eq!(decode_header_value("plain"), "plain");
    }

    #[test]
    fn strips_modern_meta() {
        let p = json!({"name":"x","inputResponses":{},"_meta":{META_PROTOCOL_VERSION:"x"}});
        let s = strip_modern_fields(&p);
        assert_eq!(s, json!({"name":"x"}));
    }
}
