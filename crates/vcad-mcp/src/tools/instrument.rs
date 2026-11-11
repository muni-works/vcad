//! Native tuned-instrument tools (the glockenspiel hero demo): design a bar
//! that rings at a note, design a whole major-scale set as an assembly, and
//! close each bar's receipt with a measured strike. Physics lives in
//! `vcad-kernel-acoustics::{instrument, instrument_doc}`; vcad.app's Tune
//! panel calls the same code through `vcad_instrument_call`.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};
use vcad_kernel_acoustics::instrument::{
    design_instrument, predicted_claim, tune_bar, BarMaterial, InstrumentSpec, TuneTarget,
};
use vcad_kernel_acoustics::instrument_doc;
use vcad_kernel_acoustics::strike::note_to_hz;
use vcad_tool_derive::ToolSchema;

use super::{ToolCtx, ToolDef, ToolError, ToolOutput, ToolSchemaEntry};
use crate::store::{CommitMeta, DocumentId};

/// Instrument tools (arguments derive `inputSchema`).
#[allow(dead_code)]
#[derive(ToolSchema)]
#[tool(category = "acoustics")]
enum InstrumentTool {
    /// Design one free-free idiophone bar (glockenspiel / marimba style) that rings at a target note. A Hermite-beam FEM with rotary inertia and shear correction predicts the modes; Newton tunes length and a central undercut until the fundamental lands within a hundredth of a cent and the first overtone sits at `overtone_ratio`×f₁. Returns geometry, nodal points (where to mount it), predicted modes and a `predicted` receipt claim.
    DesignBar {
        /// Note name, e.g. "C6", "F#5". Either this or target_hz.
        note: Option<String>,
        /// Target fundamental in Hz (instead of note).
        target_hz: Option<f64>,
        /// aluminium_6061 (default) | brass | pla.
        material: Option<String>,
        /// Bar width, mm.
        width_mm: Option<f64>,
        /// Bar thickness, mm.
        thickness_mm: Option<f64>,
        /// Target f₂/f₁ (default 4.0). 0 = plain bar, length-only tuning.
        overtone_ratio: Option<f64>,
        /// Undercut span as a fraction of length, centred (default 0.2).
        undercut_span: Option<f64>,
        /// Peg hole diameter, mm (holes go on the fundamental's nodes).
        hole_dia_mm: Option<f64>,
        /// Acceptance tolerance for the measured strike, cents (default 5).
        tolerance_cents: Option<f64>,
    },
    /// Design a tuned major-scale instrument ("a glockenspiel in C major"): eight bars tuned by FEM, laid out low to high on a frame with standoff collars and locating pegs at every bar's nodal points. Opens (or replaces) a session document whose `instrument` metadata carries one `predicted` receipt claim per bar; strike the real bars in vcad.app's Tune panel (or call record_strike) to flip them to `measured`. Also returns closed-tube resonator lengths and fabrication jobs (print plates, per-bar CAM setups).
    DesignInstrument {
        /// Lowest note; the set is its major scale (default "C6" → C6…C7).
        tonic: Option<String>,
        /// aluminium_6061 (default) | brass | pla.
        material: Option<String>,
        /// Bar width, mm.
        width_mm: Option<f64>,
        /// Bar thickness, mm.
        thickness_mm: Option<f64>,
        /// Target f₂/f₁ (default 4.0). 0 = plain bars.
        overtone_ratio: Option<f64>,
        /// Acceptance tolerance per bar, cents (default 5).
        tolerance_cents: Option<f64>,
        /// Replace this session's document instead of opening a new one.
        document_id: Option<String>,
    },
    /// Record a measured strike on a tuned instrument: the detected fundamental is matched to the nearest bar (within 60 cents) or to `note`, stored on the document, and that bar's receipt claim flips from `predicted` to `measured` (Pass inside the tolerance, Fail outside).
    RecordStrike {
        /// Session id of the instrument document.
        document_id: String,
        /// Detected fundamental, Hz.
        measured_hz: f64,
        /// Bar note (or part name) to attribute the strike to; default nearest.
        note: Option<String>,
        /// How it was measured (default "agent-reported strike").
        instrument: Option<String>,
    },
    /// Per-bar receipt for a tuned instrument: target, predicted and measured pitch, cent error and state (predicted | measured_pass | measured_fail), plus the unified receipt (Provisional until every bar is measured).
    InstrumentReceipt {
        /// Session id of the instrument document.
        document_id: String,
    },
}

fn entries() -> HashMap<String, ToolSchemaEntry> {
    InstrumentTool::tool_schemas()
        .into_iter()
        .map(|e| (e.name.clone(), e))
        .collect()
}

fn def(
    entries: &mut HashMap<String, ToolSchemaEntry>,
    name: &str,
    f: impl Fn(&ToolCtx, Value) -> Result<ToolOutput, ToolError> + Send + Sync + 'static,
) -> ToolDef {
    let entry = entries
        .remove(name)
        .unwrap_or_else(|| panic!("no schema for {name}"));
    ToolDef::from_entry(entry, None, Arc::new(f))
}

fn bad(e: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs(e.into())
}

fn num(args: &Value, k: &str) -> Option<f64> {
    args.get(k).and_then(Value::as_f64)
}

fn text<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str)
}

fn material(args: &Value) -> Result<BarMaterial, ToolError> {
    BarMaterial::parse(text(args, "material").unwrap_or("aluminium_6061")).map_err(bad)
}

/// Apply optional overrides onto a default target.
fn target_from(args: &Value, note: &str, mat: BarMaterial) -> Result<TuneTarget, ToolError> {
    let mut t = TuneTarget::for_note(note, mat).map_err(bad)?;
    if let Some(v) = num(args, "width_mm") {
        t.width_mm = v;
    }
    if let Some(v) = num(args, "thickness_mm") {
        t.thickness_mm = v;
    }
    if let Some(v) = num(args, "overtone_ratio") {
        t.overtone_ratio = (v > 0.0).then_some(v);
    }
    if let Some(v) = num(args, "undercut_span") {
        t.undercut_span = v;
    }
    if let Some(v) = num(args, "hole_dia_mm") {
        t.hole_dia_mm = v;
    }
    if let Some(v) = num(args, "tolerance_cents") {
        t.tolerance_cents = v;
    }
    Ok(t)
}

fn doc_id(args: &Value) -> Result<DocumentId, ToolError> {
    text(args, "document_id")
        .map(DocumentId::from)
        .ok_or_else(|| bad("document_id required"))
}

fn structured(v: Value) -> Result<ToolOutput, ToolError> {
    Ok(ToolOutput::structured(v))
}

/// The native instrument tool set.
pub fn instrument_tools() -> Vec<ToolDef> {
    let mut e = entries();
    vec![
        def(&mut e, "design_bar", |_ctx, args| {
            let mat = material(&args)?;
            let (note, hz) = match (text(&args, "note"), num(&args, "target_hz")) {
                (Some(n), _) => (n.to_string(), note_to_hz(n).map_err(bad)?),
                (None, Some(hz)) if hz > 0.0 => {
                    (vcad_kernel_acoustics::instrument::nearest_note(hz).0, hz)
                }
                _ => return Err(bad("pass note (e.g. \"C6\") or target_hz")),
            };
            let mut t = target_from(&args, &note, mat)?;
            t.target_hz = hz;
            let bar = tune_bar(&t).map_err(bad)?;
            let part = vcad_kernel_acoustics::instrument::part_name(&bar.note);
            let claim = predicted_claim(&bar.note, bar.target_hz, bar.hz[0], &part);
            structured(json!({
                "note": bar.note,
                "target_hz": bar.target_hz,
                "material": mat.id(),
                "predicted_hz": bar.hz,
                "cents_error": bar.cents_error,
                "overtone_ratio": bar.overtone_ratio,
                "geometry": bar.geometry,
                "nodes_mm": bar.nodes_mm,
                "shear_factor": bar.shear_factor,
                "closed_form_plain_hz": bar.closed_form_plain_hz,
                "converged": bar.converged,
                "iterations": bar.history.len(),
                "history": bar.history,
                "notes": bar.notes,
                "claim": claim,
                "loon": vcad_kernel_acoustics::instrument::bar_loon(&bar, [0.0, 0.0, 0.0]),
            }))
        }),
        def(&mut e, "design_instrument", |ctx, args| {
            let mat = material(&args)?;
            let tonic = text(&args, "tonic").unwrap_or("C6");
            let mut spec = InstrumentSpec::new(tonic, mat).map_err(bad)?;
            spec.bar = target_from(&args, tonic, mat)?;
            (ctx.progress)(0.0, Some(1.0), Some("tuning bars".into()));
            let inst = design_instrument(&spec).map_err(bad)?;
            (ctx.progress)(0.7, Some(1.0), Some("building assembly".into()));
            let mut doc = vcad_loon::eval_vcad(&inst.loon, None)
                .map_err(|e| bad(format!("generated loon failed to evaluate: {e}")))?;
            let meta = instrument_doc::metadata(&inst);
            doc.instrument = Some(meta.clone());
            let id = match text(&args, "document_id") {
                Some(id) => {
                    let id = DocumentId::from(id);
                    ctx.store
                        .commit(&id, doc, CommitMeta::agent("design_instrument"))
                        .map_err(|e| bad(e.to_string()))?;
                    id
                }
                None => ctx
                    .store
                    .create(doc, Some(&format!("{tonic} major glockenspiel")))
                    .map_err(|e| bad(e.to_string()))?,
            };
            let bars: Vec<Value> = inst
                .bars
                .iter()
                .map(|p| {
                    json!({
                        "note": p.bar.note,
                        "part": p.part,
                        "target_hz": p.bar.target_hz,
                        "predicted_hz": p.bar.hz[0],
                        "cents_error": p.bar.cents_error,
                        "overtone_ratio": p.bar.overtone_ratio,
                        "length_mm": p.bar.geometry.length_mm,
                        "undercut_depth_mm": p.bar.geometry.undercut_depth_mm,
                        "nodes_mm": p.bar.nodes_mm,
                        "resonator_mm": p.resonator_mm,
                        "notes": p.bar.notes,
                    })
                })
                .collect();
            let cam: Vec<Value> = meta
                .bars
                .iter()
                .map(|b| json!({"note": b.note, "jobs": instrument_doc::cam_jobs(b, "grbl")}))
                .collect();
            let receipt = instrument_doc::receipt_json(&meta);
            (ctx.progress)(1.0, Some(1.0), None);
            structured(json!({
                "document_id": id.0,
                "durable": ctx.store.durable(),
                "material": mat.id(),
                "bars": bars,
                "verdict": receipt["verdict"],
                "receipt": receipt["receipt"],
                "fab": {
                    "print": {
                        "plates": ["bars", "frame"],
                        "how": "vcad.app Tune panel → Print plate (dry run: writes G-code, never sends), or export_cad each plate",
                        "note": "bars print undercut-down; the 0.2·L pocket is a short bridge",
                    },
                    "mill": {
                        "post": "grbl",
                        "how": "vcad.app Tune panel → Mill bar (dry run: writes .nc files), or feed each job to vcad_cam_job",
                        "bars": cam,
                    },
                },
                "loon": inst.loon,
            }))
        }),
        def(&mut e, "record_strike", |ctx, args| {
            let id = doc_id(&args)?;
            let hz = num(&args, "measured_hz").ok_or_else(|| bad("measured_hz required"))?;
            let mut doc = ctx.store.load(&id).map_err(|e| bad(e.to_string()))?;
            let meta = doc.instrument.as_mut().ok_or_else(|| {
                bad("this document has no tuned instrument (run design_instrument)")
            })?;
            let idx = instrument_doc::record_strike(
                meta,
                hz,
                text(&args, "note"),
                text(&args, "instrument").unwrap_or("agent-reported strike"),
                None,
            )
            .map_err(bad)?;
            let bar = meta.bars[idx].clone();
            let receipt = instrument_doc::receipt_json(meta);
            ctx.store
                .commit(
                    &id,
                    doc,
                    CommitMeta::agent(format!("record_strike {}", bar.note)),
                )
                .map_err(|e| bad(e.to_string()))?;
            structured(json!({
                "note": bar.note,
                "measured_hz": hz,
                "cents_error": bar.measurement.as_ref().map(|m| m.cents_error),
                "pass": bar.passes(),
                "claim": instrument_doc::bar_claim(&bar),
                "verdict": receipt["verdict"],
                "bars": receipt["bars"],
            }))
        }),
        def(&mut e, "instrument_receipt", |ctx, args| {
            let id = doc_id(&args)?;
            let doc = ctx.store.load(&id).map_err(|e| bad(e.to_string()))?;
            let meta = doc
                .instrument
                .as_ref()
                .ok_or_else(|| bad("this document has no tuned instrument"))?;
            structured(instrument_doc::receipt_json(meta))
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::HeadlessStore;

    fn tool(name: &str) -> ToolDef {
        instrument_tools()
            .into_iter()
            .find(|t| t.name == name)
            .unwrap()
    }

    #[test]
    fn every_instrument_tool_has_schema() {
        let names: Vec<String> = instrument_tools().into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            [
                "design_bar",
                "design_instrument",
                "record_strike",
                "instrument_receipt"
            ]
        );
    }

    #[test]
    fn design_bar_hits_note() {
        let ctx = ToolCtx::basic(Arc::new(HeadlessStore::in_memory()));
        let out = (tool("design_bar").handler)(&ctx, json!({"note": "A6"})).unwrap();
        let s = out.structured.unwrap();
        assert!(s["cents_error"].as_f64().unwrap().abs() < 0.1);
        assert_eq!(s["claim"]["verdict"], "provisional");
    }

    #[test]
    fn instrument_round_trip_predicted_to_measured() {
        let ctx = ToolCtx::basic(Arc::new(HeadlessStore::in_memory()));
        let out = (tool("design_instrument").handler)(&ctx, json!({})).unwrap();
        let s = out.structured.unwrap();
        assert_eq!(s["bars"].as_array().unwrap().len(), 8);
        assert_eq!(s["verdict"], "provisional");
        let id = s["document_id"].as_str().unwrap().to_string();
        let doc = ctx.store.load(&DocumentId::from(id.as_str())).unwrap();
        assert_eq!(doc.roots.len(), 9);
        let r = (tool("record_strike").handler)(
            &ctx,
            json!({"document_id": id, "measured_hz": 1568.9}),
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(r["note"], "G6");
        assert_eq!(r["pass"], true);
        assert_eq!(r["claim"]["basis"], "measured");
        let rc = (tool("instrument_receipt").handler)(&ctx, json!({"document_id": id}))
            .unwrap()
            .structured
            .unwrap();
        assert_eq!(rc["bars"][4]["state"], "measured_pass");
        assert_eq!(rc["bars"][0]["state"], "predicted");
    }
}
