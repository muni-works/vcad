//! Host tools: the ones that need vcad.app's viewport, panels or user —
//! camera, pose, highlights, snapshots, the elicitations answered in the
//! viewport, the `@vcad/mcp` link handshake. The core owns their listing
//! (`host_tools.json`) and dispatch; the app executes them through
//! [`DocumentStore::host_call`](crate::store::DocumentStore::host_call).
//!
//! Headless lists them too (one tool surface in both modes) and answers with a
//! clear `isError`, or hands them to node where node has its own version.

use std::sync::Arc;

use serde_json::{json, Value};

use super::{ToolCtx, ToolDef, ToolError, ToolOutput};

/// Host tool definitions (`name`, `description`, `inputSchema`, optional
/// `outputSchema` / `_meta`). Checked against the Swift side by
/// `HostToolsTests` in the app.
pub const HOST_TOOLS_JSON: &str = include_str!("host_tools.json");

/// Host tools the node server also implements: headless delegates these.
pub const NODE_ALSO: &[&str] = &[
    "analyze_structure",
    "dfm_apply_fix",
    "dfm_check",
    "inspect_faces",
    "open_in_app",
];

/// Host-internal call made before a native `delete` on a document carrying
/// receipts: the app confirms in-app (or takes the MRTR answer).
pub const CONFIRM_DESTRUCTIVE: &str = "__confirm_destructive";

/// Parsed definitions.
pub fn definitions() -> Vec<Value> {
    serde_json::from_str(HOST_TOOLS_JSON).expect("host_tools.json is valid JSON")
}

/// Names of every host tool.
pub fn names() -> Vec<String> {
    definitions()
        .iter()
        .filter_map(|d| d["name"].as_str().map(str::to_string))
        .collect()
}

/// A raw `CallToolResult` from the host → [`ToolOutput`].
pub fn output_from_result(v: &Value) -> ToolOutput {
    ToolOutput {
        content: v["content"].as_array().cloned().unwrap_or_default(),
        structured: v.get("structuredContent").filter(|s| !s.is_null()).cloned(),
        is_error: v["isError"].as_bool().unwrap_or(false),
    }
}

fn run(ctx: &ToolCtx, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
    match ctx.store.host_call(name, &args, &ctx.input_responses) {
        Some(Ok(v)) => Ok(output_from_result(&v)),
        Some(Err(e)) => Ok(ToolOutput::error(e.to_string())),
        None if NODE_ALSO.contains(&name) => {
            Err(ToolError::Delegate(format!("{name}: no vcad.app attached")))
        }
        None => Ok(ToolOutput::error(format!(
            "`{name}` drives the vcad.app viewport, and this server is running headless. Open vcad.app (`vcad mcp` attaches to it automatically) or use the document tools."
        ))),
    }
}

/// One `ToolDef` per host tool.
pub fn tools() -> Vec<ToolDef> {
    definitions()
        .into_iter()
        .filter_map(|d| {
            let name = d["name"].as_str()?.to_string();
            let n2 = name.clone();
            Some(ToolDef {
                name,
                description: d["description"].as_str().unwrap_or("").to_string(),
                input_schema: d
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                output_schema: d.get("outputSchema").cloned(),
                long_running: false,
                handler: Arc::new(move |ctx, args| run(ctx, &n2, args)),
            })
        })
        .collect()
}

/// `_meta` for a host tool's listing (the live viewer on `fillet_edges`).
pub fn listing_meta(name: &str) -> Option<Value> {
    definitions()
        .into_iter()
        .find(|d| d["name"] == name)
        .and_then(|d| d.get("_meta").cloned())
}
