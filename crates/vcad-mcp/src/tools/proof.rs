//! Receipt tools (M2), native over `vcad-receipt-verify`: `assert_clearance`
//! persists a clearance claim on the document, `verify_receipts` re-measures
//! every persisted claim, `accept_receipts` re-baselines holding ones. Each
//! returns the receipt as structured output (status per claim plus the
//! unified `vcad.receipt/1` DesignReceipt). vcad.app's viewport badges follow
//! the document, so they update from the same commits.

use std::sync::Arc;

use serde_json::{json, Value};
use vcad_ir::ClearanceSpec;

use super::core::{resolve_id, store_err};
use super::{ToolCtx, ToolDef, ToolError, ToolOutput};
use crate::store::CommitMeta;

/// `outputSchema` shared by the receipt tools.
pub fn receipt_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": {"type": ["string", "null"], "enum": ["Holds", "Stale", "Violated", null],
                       "description": "Worst status across claims; null when nothing is asserted."},
            "checks": {"type": "array", "items": {
                "type": "object",
                "properties": {
                    "label": {"type": "string"}, "claim_id": {"type": "string"},
                    "status": {"type": "string", "enum": ["Holds", "Stale", "Violated"]},
                    "subject": {"type": "string"}, "required_mm": {"type": "number"},
                    "measured_mm": {"type": "number"}, "baseline_mm": {"type": "number"},
                    "worst_pair": {"type": "array", "items": {"type": "string"}},
                    "reason": {"type": "string"}
                },
                "required": ["label", "claim_id", "status", "subject", "required_mm"]
            }},
            "receipt": {"type": "object", "description": "Unified vcad.receipt/1 DesignReceipt."}
        },
        "required": ["status", "checks", "receipt"]
    })
}

fn report_output(doc: &vcad_ir::Document, prefix: Option<String>) -> Result<ToolOutput, ToolError> {
    let report = vcad_receipt_verify::verify(doc)
        .map_err(|e| ToolError::InvalidArgs(format!("Receipt verification failed: {e}")))?;
    let mut v = serde_json::to_value(&report).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
    // Drop the viewport leader points from the agent-facing checks.
    if let Some(checks) = v["checks"].as_array_mut() {
        for c in checks {
            if let Some(o) = c.as_object_mut() {
                o.remove("points");
            }
        }
    }
    let mut out = ToolOutput::structured(v);
    if let Some(p) = prefix {
        if let Some(Value::Object(t)) = out.content.first_mut() {
            let body = t
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            t.insert("text".into(), json!(format!("{p}\n{body}")));
        }
    }
    Ok(out)
}

fn def(
    name: &str,
    description: &str,
    input_schema: Value,
    f: impl Fn(&ToolCtx, Value) -> Result<ToolOutput, ToolError> + Send + Sync + 'static,
) -> ToolDef {
    ToolDef {
        name: name.into(),
        description: description.into(),
        input_schema,
        output_schema: Some(receipt_output_schema()),
        long_running: false,
        handler: Arc::new(f),
    }
}

fn strings(v: &Value) -> Option<Vec<String>> {
    let a: Vec<String> = v
        .as_array()?
        .iter()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect();
    (!a.is_empty()).then_some(a)
}

/// `assert_clearance`, `verify_receipts`, `accept_receipts`.
pub fn tools() -> Vec<ToolDef> {
    let doc_id = json!({"type": "string", "description": "Session id; omit for the current document (the open vcad.app window)."});
    vec![
        def(
            "assert_clearance",
            "Persist a clearance claim on the document: the minimum distance between part group_a and group_b (part names or root node ids) must stay ≥ min_mm. In vcad.app it becomes a live badge that re-verifies on every edit (Holds green / Stale gray / Violated red). Returns the receipt as structured output. Re-asserting a label replaces it. One undo step.",
            json!({"type": "object", "properties": {
                "document_id": doc_id.clone(),
                "label": {"type": "string"},
                "group_a": {"type": "array", "items": {"type": "string"}},
                "group_b": {"type": "array", "items": {"type": "string"}},
                "min_mm": {"type": "number"},
                "allow_contact": {"type": "boolean", "description": "Flush contact (distance ≈ 0) satisfies the claim."}
            }, "required": ["label", "group_a", "group_b", "min_mm"]}),
            |ctx, a| {
                let label = a["label"].as_str().filter(|l| !l.is_empty());
                let (Some(label), Some(ga), Some(gb), Some(min)) =
                    (label, strings(&a["group_a"]), strings(&a["group_b"]), a["min_mm"].as_f64())
                else {
                    return Err(ToolError::InvalidArgs(
                        "label, group_a, group_b and min_mm are required".into(),
                    ));
                };
                let id = resolve_id(ctx, &a)?;
                let mut doc = ctx.store.load(&id).map_err(store_err)?;
                doc.clearance_specs.retain(|s| s.label != label);
                doc.clearance_specs.push(ClearanceSpec {
                    label: label.to_string(),
                    group_a: ga,
                    group_b: gb,
                    min_mm: min,
                    allow_contact: a["allow_contact"].as_bool().filter(|b| *b),
                    sweep: None,
                    baseline_mm: None,
                });
                ctx.store
                    .commit(&id, doc.clone(), CommitMeta::agent(format!("Assert clearance “{label}”")))
                    .map_err(store_err)?;
                report_output(&doc, None)
            },
        ),
        def(
            "verify_receipts",
            "Re-measure every claim persisted on the document against the current geometry. Violated = fails or can't be measured (fail-closed); Stale = holds but drifted from its accepted measurement; Holds = holds and matches. Returns per-claim status and the unified DesignReceipt as structured output. Never report a claim as true without this.",
            json!({"type": "object", "properties": {"document_id": doc_id.clone()}}),
            |ctx, a| {
                let id = resolve_id(ctx, &a)?;
                let doc = ctx.store.load(&id).map_err(store_err)?;
                report_output(&doc, None)
            },
        ),
        def(
            "accept_receipts",
            "Accept the current measurements as the baseline for holding claims (label, or all), turning Stale back to Holds. A Violated claim is never accepted. One undo step.",
            json!({"type": "object", "properties": {"document_id": doc_id, "label": {"type": "string"}}}),
            |ctx, a| {
                let id = resolve_id(ctx, &a)?;
                let mut doc = ctx.store.load(&id).map_err(store_err)?;
                let n = vcad_receipt_verify::accept(&mut doc, a["label"].as_str())
                    .map_err(|e| ToolError::InvalidArgs(format!("Receipt verification failed: {e}")))?;
                if n > 0 {
                    ctx.store
                        .commit(&id, doc.clone(), CommitMeta::agent("Accept receipts"))
                        .map_err(store_err)?;
                }
                report_output(&doc, Some(format!("Accepted {n} claim{}.", if n == 1 { "" } else { "s" })))
            },
        ),
    ]
}
