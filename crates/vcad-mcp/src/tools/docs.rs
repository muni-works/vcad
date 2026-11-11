//! Document-level native tools the Swift bridge and the node server both had:
//! `list_parameters`, `set_parameters`, `undo`. They act on `document_id`, or
//! on the store's current document (vcad.app: the front window) when omitted.

use std::sync::Arc;

use serde_json::{json, Value};
use vcad_ir::Expr;

use super::core::{resolve_id, store_err};
use super::{ToolCtx, ToolDef, ToolError, ToolOutput};
use crate::store::CommitMeta;

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

fn doc_id_prop() -> Value {
    json!({"type": "string", "description": "Session id; omit for the current document (the open vcad.app window)."})
}

fn tool(
    name: &str,
    description: &str,
    input_schema: Value,
    f: impl Fn(&ToolCtx, Value) -> Result<ToolOutput, ToolError> + Send + Sync + 'static,
) -> ToolDef {
    ToolDef {
        name: name.into(),
        description: description.into(),
        input_schema,
        output_schema: None,
        long_running: false,
        handler: Arc::new(f),
    }
}

/// Parameter rows: name, value (literal), settable, min/max/unit.
pub fn parameter_rows(doc: &vcad_ir::Document) -> Vec<Value> {
    let mut names: Vec<&String> = doc.parameters.keys().collect();
    names.sort();
    names
        .into_iter()
        .map(|n| {
            let p = &doc.parameters[n];
            let mut o = json!({ "name": n, "settable": matches!(p.value, Expr::Number(_)) });
            match &p.value {
                Expr::Number(v) => o["value"] = json!(v),
                other => o["formula"] = json!(other),
            }
            if let Some(v) = p.min {
                o["min"] = json!(v);
            }
            if let Some(v) = p.max {
                o["max"] = json!(v);
            }
            if let Some(u) = &p.unit {
                o["unit"] = json!(u);
            }
            o
        })
        .collect()
}

/// `list_parameters`, `set_parameters`, `undo`.
pub fn tools() -> Vec<ToolDef> {
    vec![
        tool(
            "list_parameters",
            "Document-level named parameters ([defparam …] in loon): value, unit, range, and whether set_parameters can change them (formulas can't).",
            obj(json!({ "document_id": doc_id_prop(), "document": {"type": "object"} }), &[]),
            |ctx, args| {
                if args.get("document").is_some_and(Value::is_object) {
                    return Err(ToolError::Delegate("inline document is served by node".into()));
                }
                let id = resolve_id(ctx, &args)?;
                let doc = ctx.store.load(&id).map_err(store_err)?;
                Ok(ToolOutput::structured(json!({ "document_id": id.0, "parameters": parameter_rows(&doc) })))
            },
        ),
        tool(
            "set_parameters",
            "Set named document parameters, e.g. {\"wall\": 3.5}. Values clamp to their declared range; every bound dimension updates. One undo step.",
            obj(
                json!({
                    "document_id": doc_id_prop(),
                    "parameters": {"type": "object", "additionalProperties": {"type": "number"}, "description": "name → value"},
                    "values": {"type": "object", "additionalProperties": {"type": "number"}, "description": "Alias of parameters."},
                }),
                &[],
            ),
            |ctx, args| {
                let values = args
                    .get("parameters")
                    .or_else(|| args.get("values"))
                    .and_then(Value::as_object)
                    .filter(|m| !m.is_empty())
                    .ok_or_else(|| {
                        ToolError::InvalidArgs("parameters is required, e.g. {\"wall\": 3}".into())
                    })?;
                let id = resolve_id(ctx, &args)?;
                let mut doc = ctx.store.load(&id).map_err(store_err)?;
                let mut applied: Vec<(String, f64)> = Vec::new();
                let mut refused: Vec<String> = Vec::new();
                for (k, v) in values {
                    let (Some(p), Some(x)) = (doc.parameters.get_mut(k), v.as_f64()) else {
                        refused.push(format!("{k} (unknown)"));
                        continue;
                    };
                    if !matches!(p.value, Expr::Number(_)) {
                        refused.push(format!("{k} (formula)"));
                        continue;
                    }
                    let mut c = x;
                    if let Some(lo) = p.min {
                        c = c.max(lo);
                    }
                    if let Some(hi) = p.max {
                        c = c.min(hi);
                    }
                    p.value = Expr::Number(c);
                    applied.push((k.clone(), c));
                }
                if applied.is_empty() {
                    let mut known: Vec<&String> = doc.parameters.keys().collect();
                    known.sort();
                    return Ok(ToolOutput::error(format!(
                        "No settable parameter in {}. Known: {}",
                        refused.join(", "),
                        known.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                    )));
                }
                applied.sort_by(|a, b| a.0.cmp(&b.0));
                let label = if applied.len() == 1 {
                    format!("Set {}", applied[0].0)
                } else {
                    "Set Parameters".into()
                };
                ctx.store
                    .commit(&id, doc, CommitMeta::agent(label))
                    .map_err(store_err)?;
                let mut msg = format!(
                    "Set {}",
                    applied
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                if !refused.is_empty() {
                    msg.push_str(&format!(". Refused: {}", refused.join(", ")));
                }
                let mut out = ToolOutput::structured(json!({
                    "document_id": id.0,
                    "applied": applied.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<_, _>>(),
                    "refused": refused,
                }));
                out.content.insert(0, json!({"type": "text", "text": msg}));
                Ok(out)
            },
        ),
        tool(
            "undo",
            "Undo the last edit to the document (in vcad.app: yours or the user's, exactly like ⌘Z).",
            obj(json!({ "document_id": doc_id_prop() }), &[]),
            |ctx, args| {
                let id = resolve_id(ctx, &args)?;
                match ctx.store.undo(&id) {
                    Ok(label) => Ok(ToolOutput::structured(json!({ "document_id": id.0, "undone": label }))),
                    // Headless keeps only this process's native commits; an
                    // edit the node server made is node's to undo.
                    Err(crate::store::StoreError::Unavailable(_))
                        if ctx.store.kind() == crate::store::StoreKind::Headless =>
                    {
                        Err(ToolError::Delegate("no native undo step".into()))
                    }
                    Err(crate::store::StoreError::Unavailable(why)) => {
                        Ok(ToolOutput::error(format!("Nothing undone: {why}.")))
                    }
                    Err(e) => Err(store_err(e)),
                }
            },
        ),
    ]
}
