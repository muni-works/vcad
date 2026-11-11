//! M4's `vcad-atoms` jobs as native long-running tools: `fab_print`,
//! `fab_mill`, `optimize_parameters`. Each runs on its own thread; task-capable
//! clients get an MCP Task (progress + `tasks/cancel`), others block on the
//! call. Fabrication streams only to dry-run machines — never real hardware.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use vcad_atoms::task::TaskStatus;

use super::core::{resolve_doc, store_err};
use super::{ToolCtx, ToolDef, ToolError, ToolOutput};

fn run(ctx: &ToolCtx, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
    let doc = resolve_doc(ctx, &args).or_else(|e| match ctx.store.current() {
        Some(id) => ctx.store.load(&id).map_err(store_err),
        None => Err(e),
    })?;
    let mut job_args = args.clone();
    if let Some(o) = job_args.as_object_mut() {
        o.remove("document_id");
        o.remove("document");
    }
    let handle = vcad_atoms::spawn_tool(name, doc, job_args)
        .ok_or_else(|| ToolError::InvalidArgs(format!("unknown atoms tool {name}")))?;
    let mut seen = 0usize;
    let snap = loop {
        if ctx.cancel.load(Ordering::Relaxed) {
            handle.cancel();
        }
        let events = handle.events();
        for e in events.iter().skip(seen) {
            (ctx.progress)(e.progress, Some(1.0), Some(e.message.clone()));
        }
        seen = events.len();
        let s = handle.snapshot();
        if s.status.is_terminal() {
            break handle.wait();
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    match snap.status {
        TaskStatus::Completed => Ok(ToolOutput::structured(snap.result.unwrap_or(Value::Null))),
        TaskStatus::Cancelled => Ok(ToolOutput::error(format!("{name} cancelled"))),
        _ => {
            // Fail closed, with the gate report when there is one.
            let mut out = ToolOutput::error(format!(
                "{name} failed: {}",
                snap.error.unwrap_or_else(|| "unknown error".into())
            ));
            if let Some(r) = snap.result {
                out.content.push(json!({"type": "text", "text": serde_json::to_string_pretty(&r).unwrap_or_default()}));
                out.structured = Some(r);
            }
            Ok(out)
        }
    }
}

/// The `vcad-atoms` tool set, schemas from the crate's descriptors plus an
/// optional `document_id`.
pub fn tools() -> Vec<ToolDef> {
    vcad_atoms::tool_descriptors()
        .into_iter()
        .filter_map(|d| {
            let name = d["name"].as_str()?.to_string();
            let mut schema = d["inputSchema"].clone();
            if let Some(p) = schema["properties"].as_object_mut() {
                p.insert(
                    "document_id".into(),
                    json!({"type": "string", "description": "Session id; omit for the current document (the open vcad.app window)."}),
                );
            }
            let n2 = name.clone();
            Some(ToolDef {
                name,
                description: d["description"].as_str().unwrap_or("").to_string(),
                input_schema: schema,
                output_schema: None,
                long_running: true,
                handler: Arc::new(move |ctx, args| run(ctx, &n2, args)),
            })
        })
        .collect()
}
