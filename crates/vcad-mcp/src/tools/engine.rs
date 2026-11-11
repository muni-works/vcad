//! The engine's agent tools (`crates/vcad-app/agent/manifest.json`: `create`,
//! `update`, `delete`, `set_material`, booleans, transforms, patterns, plus the
//! `describe_scene` / `inspect` reads), served natively over the store.
//!
//! Before Phase A, vcad.app ran these through Swift (`AgentEdits` →
//! `vcad_engine_apply_tool`) and the node server had its own `create` /
//! `update` / `delete` / `set_material` / `describe_scene`. Now one
//! implementation: load the document, apply `vcad_app`'s lowering, commit the
//! result as one named undo step (in the app: live in the viewport).
//!
//! Arguments outside the engine's vocabulary (node-only primitive types,
//! `describe_scene` with `joint_state`, an inline `document`) delegate to the
//! node server unchanged, so no headless agent sees a behaviour change.

use std::sync::Arc;

use serde_json::{json, Value};
use vcad_app::DocumentApi;
use vcad_crdt::ReplicaId;
use vcad_ir::Document;

use super::core::{resolve_id, store_err};
use super::{ToolCtx, ToolDef, ToolError, ToolOutput};
use crate::store::{CommitMeta, DocumentId, StoreKind};

/// Read tools (no commit).
pub const READ_TOOLS: &[&str] = &["describe_scene", "inspect"];

/// Engine tools not served here: `render_view` is the core's own renderer.
const SKIPPED: &[&str] = &["render_view"];

/// Replica id for engines the server builds per call.
const SERVER_REPLICA: u64 = 0x6d63_7000; // "mcp\0"

/// `create` → "Create Bracket", `difference` → "Difference", with the part
/// name when one is given (the app prefixes the agent's display name).
pub fn undo_label(tool: &str, args: &Value) -> String {
    let verb: String = tool
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ");
    let name = args.get("name").and_then(Value::as_str).filter(|n| !n.is_empty());
    if tool == "create" {
        let label = name.map(str::to_string).unwrap_or_else(|| {
            args.get("type")
                .and_then(Value::as_str)
                .map(|t| {
                    let mut c = t.chars();
                    c.next()
                        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                        .unwrap_or_default()
                })
                .unwrap_or_else(|| "Part".into())
        });
        return format!("Create {label}");
    }
    match name {
        Some(n) => format!("{verb} {n}"),
        None => verb,
    }
}

fn engine_for(doc: &Document) -> Result<DocumentApi, ToolError> {
    if doc.nodes.is_empty() && doc.roots.is_empty() {
        return Ok(DocumentApi::new(ReplicaId(SERVER_REPLICA)));
    }
    let json = serde_json::to_string(doc).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    DocumentApi::from_ir(&json, ReplicaId(SERVER_REPLICA))
        .map_err(|e| ToolError::Delegate(format!("engine cannot load this document: {e}")))
}

/// The document a call acts on. With no `document_id` and nothing current, a
/// fresh empty document is created (the app's empty Untitled window gets one
/// on first use the same way).
fn target(ctx: &ToolCtx, args: &Value) -> Result<DocumentId, ToolError> {
    match resolve_id(ctx, args) {
        Ok(id) => Ok(id),
        Err(_) => ctx
            .store
            .create(Document::default(), None)
            .map_err(store_err),
    }
}

/// Engine args: everything but the routing keys.
fn engine_args(args: &Value) -> Value {
    let mut a = args.clone();
    if let Some(o) = a.as_object_mut() {
        o.remove("document_id");
    }
    a
}

fn envelope_error(env: &Value) -> String {
    let e = &env["error"];
    let kind = e.get("kind").and_then(Value::as_str).unwrap_or("error");
    let msg = e.get("message").and_then(Value::as_str).unwrap_or("refused");
    format!("{kind}: {msg}")
}

/// A deletion may break the document's receipts: in vcad.app the user
/// confirms (natively, or via the client's MRTR answer). Headless proceeds.
fn confirm_delete(ctx: &ToolCtx, doc: &Document, args: &Value) -> Option<ToolOutput> {
    let part = args["part_id"].as_str().unwrap_or("a part");
    let labels: Vec<&str> = doc.clearance_specs.iter().map(|s| s.label.as_str()).collect();
    let n = labels.len();
    let req = json!({
        "part_id": part,
        "message": format!(
            "The agent wants to delete {part}. This document has {n} receipt{} ({}) that may stop verifying.",
            if n == 1 { "" } else { "s" },
            labels.join(", ")
        ),
    });
    let answer = ctx
        .store
        .host_call(super::host::CONFIRM_DESTRUCTIVE, &req, &ctx.input_responses)?;
    let action = answer
        .ok()
        .and_then(|v| v["action"].as_str().map(str::to_string))
        .unwrap_or_else(|| "cancel".into());
    (action != "accept").then(|| {
        ToolOutput::error(format!(
            "The user {} deleting {part} — nothing changed.",
            if action == "decline" { "declined" } else { "cancelled" }
        ))
    })
}

fn run(ctx: &ToolCtx, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
    if args.get("document").is_some_and(Value::is_object) {
        return Err(ToolError::Delegate("inline document is served by node".into()));
    }
    if name == "describe_scene"
        && ["joint_state", "part_ids", "limit"]
            .iter()
            .any(|k| args.get(*k).is_some())
    {
        return Err(ToolError::Delegate(format!("{name}: filters are served by node")));
    }
    let headless = ctx.store.kind() == StoreKind::Headless;
    let id = target(ctx, &args)?;
    let doc = ctx.store.load(&id).map_err(store_err)?;
    if name == "delete" && !doc.clearance_specs.is_empty() {
        if let Some(refusal) = confirm_delete(ctx, &doc, &args) {
            return Ok(refusal);
        }
    }
    let a = engine_args(&args);
    let label = undo_label(name, &args);
    let read = READ_TOOLS.contains(&name);
    // vcad.app applies the tool on the window's own engine (same lowering,
    // its feature history and undo); headless uses a cached engine.
    let (env, committed) = match ctx.store.engine_tool(&id, name, &a, &label) {
        Some(r) => (r.map_err(store_err)?, true),
        None => (local_engine_tool(&id, &doc, name, &a, read)?, false),
    };
    if read {
        if env["ok"] != json!(true) {
            return Ok(ToolOutput::error(envelope_error(&env)));
        }
        let text = env["result"]["text"].as_str().unwrap_or("").to_string();
        return Ok(ToolOutput {
            content: vec![json!({"type": "text", "text": text})],
            structured: Some(json!({"document_id": id.0, "text": text})),
            is_error: false,
        });
    }
    if env["ok"] != json!(true) {
        let err = envelope_error(&env);
        // The engine lowering doesn't know node's richer vocabulary (extra
        // primitive types, fields): let node answer exactly as before.
        let unsupported = matches!(
            env["error"]["kind"].as_str(),
            Some("unsupported" | "invalid" | "unknown_tool")
        );
        if headless && unsupported {
            return Err(ToolError::Delegate(err));
        }
        return Ok(ToolOutput::error(err));
    }
    let result = &env["result"];
    if !committed {
        let new_doc: Document = serde_json::from_value(result["document"].clone()).map_err(|e| {
            ToolError::InvalidArgs(format!("engine produced an invalid document: {e}"))
        })?;
        remember(&id, &new_doc, None);
        ctx.store
            .commit(&id, new_doc, CommitMeta::agent(label.clone()))
            .map_err(store_err)?;
    }
    let parts = result["parts"].as_array().map_or(0, Vec::len);
    let created = result["createdFeatureId"].as_str().map(str::to_string);
    let warnings: Vec<String> = result["warnings"]
        .as_array()
        .map(|w| w.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let mut text = "ok".to_string();
    if let Some(c) = &created {
        text.push_str(&format!("; created part {c}"));
    }
    text.push_str(&format!(
        "; document has {parts} part{}",
        if parts == 1 { "" } else { "s" }
    ));
    if !warnings.is_empty() {
        text.push_str(&format!("; warnings: {}", warnings.join("; ")));
    }
    text.push_str(&format!(". Undo label: \"{label}\"."));
    let mut structured = json!({
        "document_id": id.0,
        "parts": parts,
        "warnings": warnings,
        "undo_label": label,
    });
    if let Some(c) = created {
        structured["created_part_id"] = json!(c);
    }
    if let Some(c) = result.get("consumedPartIds") {
        structured["consumed_part_ids"] = c.clone();
    }
    Ok(ToolOutput {
        content: vec![json!({"type": "text", "text": text})],
        structured: Some(structured),
        is_error: false,
    })
}

/// Headless: one live engine per document, reused while the stored document
/// is exactly what that engine last produced (so feature state — consumed
/// boolean inputs, stable part ids — survives between calls). Anything else
/// changed the document (node, a reload): rebuild from its IR.
type EngineCache = std::collections::HashMap<DocumentId, (String, DocumentApi)>;

fn cache() -> &'static std::sync::Mutex<EngineCache> {
    static C: std::sync::OnceLock<std::sync::Mutex<EngineCache>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

fn doc_key(doc: &Document) -> String {
    serde_json::to_string(doc).unwrap_or_default()
}

fn remember(id: &DocumentId, doc: &Document, api: Option<DocumentApi>) {
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    match api {
        Some(api) => {
            c.insert(id.clone(), (doc_key(doc), api));
        }
        None => {
            if let Some(e) = c.get_mut(id) {
                e.0 = doc_key(doc);
            }
        }
    }
}

fn local_engine_tool(
    id: &DocumentId,
    doc: &Document,
    name: &str,
    args: &Value,
    read: bool,
) -> Result<Value, ToolError> {
    let key = doc_key(doc);
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    let fresh = !matches!(c.get(id), Some((k, _)) if *k == key);
    if fresh {
        c.insert(id.clone(), (key, engine_for(doc)?));
    }
    let api = &mut c.get_mut(id).expect("just inserted").1;
    let a = args.to_string();
    let raw = if read {
        vcad_app::agent::read_tool_envelope(api, name, &a)
    } else {
        api.apply_tool_envelope(name, &a)
    };
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

/// One `ToolDef` per manifest tool, schemas from the manifest plus an
/// optional `document_id`.
pub fn tools() -> Vec<ToolDef> {
    let manifest: Value = serde_json::from_str(vcad_app::agent::MANIFEST_JSON).unwrap_or_default();
    let mut all: Vec<Value> = Vec::new();
    for key in ["tools", "vision_tools"] {
        if let Some(a) = manifest[key].as_array() {
            all.extend(a.iter().cloned());
        }
    }
    all.into_iter()
        .filter_map(|t| {
            let f = t.get("function").cloned().unwrap_or(t);
            let name = f["name"].as_str()?.to_string();
            if SKIPPED.contains(&name.as_str()) {
                return None;
            }
            let mut schema = f
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            if let Some(p) = schema
                .as_object_mut()
                .map(|o| o.entry("properties").or_insert_with(|| json!({})))
                .and_then(Value::as_object_mut)
            {
                p.insert(
                    "document_id".into(),
                    json!({"type": "string", "description": "Session id; omit for the current document (the open vcad.app window)."}),
                );
            }
            let mut description = f["description"].as_str().unwrap_or("").to_string();
            if !READ_TOOLS.contains(&name.as_str()) {
                description.push_str(" One named undo step (live in the vcad.app viewport).");
            }
            let n2 = name.clone();
            Some(ToolDef {
                name,
                description,
                input_schema: schema,
                output_schema: None,
                long_running: false,
                handler: Arc::new(move |ctx, args| run(ctx, &n2, args)),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{DocumentStore, HeadlessStore};

    fn call(ctx: &ToolCtx, name: &str, args: Value) -> ToolOutput {
        let t = tools().into_iter().find(|t| t.name == name).unwrap();
        (t.handler)(ctx, args).unwrap_or_else(|e| panic!("{name}: {e:?}"))
    }

    #[test]
    fn labels() {
        assert_eq!(undo_label("create", &json!({"name": "Bracket"})), "Create Bracket");
        assert_eq!(undo_label("create", &json!({"type": "cube"})), "Create Cube");
        assert_eq!(undo_label("linear_pattern", &json!({})), "Linear Pattern");
    }

    #[test]
    fn create_then_update_round_trips_through_ir() {
        let store: Arc<dyn DocumentStore> = Arc::new(HeadlessStore::in_memory());
        let ctx = ToolCtx::basic(store.clone());
        let out = call(&ctx, "create", json!({"type": "cube", "name": "Block", "params": {"size": {"x": 10, "y": 20, "z": 5}}}));
        assert!(!out.is_error, "{:?}", out.content);
        let s = out.structured.unwrap();
        let id = DocumentId::from(s["document_id"].as_str().unwrap());
        let part = s["created_part_id"].as_str().unwrap().to_string();
        assert_eq!(s["parts"], 1);
        // A second call loads the committed IR into a fresh engine and must
        // still find the part by the id the first call returned.
        let out = call(&ctx, "create", json!({"type": "cylinder", "params": {"radius": 2, "height": 30}}));
        assert!(!out.is_error, "{:?}", out.content);
        let cyl = out.structured.unwrap()["created_part_id"].as_str().unwrap().to_string();
        let out = call(&ctx, "difference", json!({"left": part, "right": cyl}));
        assert!(!out.is_error, "{:?}", out.content);
        // Both inputs consumed by the boolean (parts lists every feature).
        let s = out.structured.unwrap();
        assert_eq!(s["consumed_part_ids"].as_array().map(Vec::len), Some(2), "{s}");
        let doc = store.load(&id).unwrap();
        assert!(!doc.roots.is_empty());
        let out = call(&ctx, "describe_scene", json!({}));
        assert!(!out.is_error);
        // Undo walks the native commits back one at a time.
        assert_eq!(store.undo(&id).unwrap(), "Difference");
    }
}
