//! The physics solver tools, served natively (no wasm, no node):
//! `simulate_charged_particles`, `optimize_electrodes`,
//! `simulate_neutron_shield` and `compare_measurements`, all from
//! `vcad-solver-tools` — the same implementation vcad.app's Analysis →
//! Fields panel calls through `vcad-ffi`.
//!
//! The agent gets a compacted result (no potential grid, tracks or
//! contours; the last few optimizer evaluations), since a structured
//! result is pretty-printed into its context. Inside vcad.app the FULL
//! result is handed to the app as the host-internal [`SHOW_RESULT`] call,
//! so what the agent ran lands in the panel and the viewport. Headless has
//! no host and the call is a no-op.

use std::sync::Arc;

use serde_json::{json, Value};
use vcad_eval::sim_extract;
use vcad_solver_tools as st;

use super::{ToolCtx, ToolDef, ToolError, ToolOutput};
use crate::store::DocumentId;

/// Host-internal call carrying a finished solver result to vcad.app
/// (`{tool, args, result}`); never listed.
pub const SHOW_RESULT: &str = "__show_solver_result";

/// The tools that accept `document_id` + `extract` instead of a raw `spec`
/// (the same contract as the node server's #1187 path).
const DOC_TOOLS: [&str; 3] = [
    "simulate_charged_particles",
    "optimize_electrodes",
    "simulate_neutron_shield",
];

/// Resolve a `document_id` + `extract` call into a raw-spec call: measure
/// the session document's solids with `vcad_eval::sim_extract`, bind the
/// document's parameters (caller `parameters` override them) and return the
/// rewritten args plus the `extracted` audit block echoed in the result.
/// Calls without `document_id` pass through untouched.
fn resolve_document(
    ctx: &ToolCtx,
    name: &str,
    args: Value,
) -> Result<(Value, Option<Value>), String> {
    let Some(id) = args.get("document_id") else {
        return Ok((args, None));
    };
    if !DOC_TOOLS.contains(&name) {
        return Ok((args, None));
    }
    let id = id
        .as_str()
        .ok_or("`document_id` must be a string")?
        .to_string();
    if args.get("spec").is_some() {
        return Err("pass `spec` OR `document_id`, not both".into());
    }
    let sel = args.get("extract").cloned().ok_or_else(|| {
        if name == "simulate_neutron_shield" {
            "`document_id` requires `extract` (shell parts, source, detectors)".to_string()
        } else {
            "`document_id` requires `extract` (chamber + electrode parts)".to_string()
        }
    })?;
    let doc = ctx
        .store
        .load(&DocumentId::from(id.as_str()))
        .map_err(|e| e.to_string())?;
    let mut ex = if name == "simulate_neutron_shield" {
        let sel: sim_extract::ShieldExtraction =
            serde_json::from_value(sel).map_err(|e| format!("bad `extract`: {e}"))?;
        let out = sim_extract::extract_shield_spec(&doc, &sel).map_err(|e| e.to_string())?;
        serde_json::to_value(out).map_err(|e| e.to_string())?
    } else {
        let sel: sim_extract::DeviceExtraction =
            serde_json::from_value(sel).map_err(|e| format!("bad `extract`: {e}"))?;
        let out = sim_extract::extract_device_spec(&doc, &sel).map_err(|e| e.to_string())?;
        serde_json::to_value(out).map_err(|e| e.to_string())?
    };
    let mut parameters = ex
        .get("parameters")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(over) = args.get("parameters").and_then(Value::as_object) {
        parameters.extend(over.clone());
    }
    let parameters = Value::Object(parameters);
    let spec = ex.get("spec").cloned().unwrap_or(Value::Null);
    let mut out = args;
    if let Some(o) = out.as_object_mut() {
        o.remove("document_id");
        o.remove("extract");
        o.insert("spec".into(), spec);
        o.insert("parameters".into(), parameters.clone());
    }
    if let Some(o) = ex.as_object_mut() {
        o.insert("parameters".into(), parameters);
        o.insert("document_id".into(), Value::String(id));
    }
    Ok((out, Some(ex)))
}

fn run(ctx: &ToolCtx, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
    let (mut args, extracted) = match resolve_document(ctx, name, args) {
        Ok(r) => r,
        Err(m) => return Ok(ToolOutput::error(format!("{name}: invalid arguments: {m}"))),
    };
    // The section is cheap next to the trace and the agent never sees it
    // (compacted below), but vcad.app draws it: compute it unless asked not to.
    if name == "simulate_charged_particles" && args.get("section").is_none() {
        if let Some(o) = args.as_object_mut() {
            o.insert("section".into(), Value::Bool(true));
        }
    }
    let progress_fn = ctx.progress.clone();
    let progress = move |f: f64, msg: &str| (progress_fn)(f, Some(1.0), Some(msg.to_string()));
    let hooks = st::Hooks {
        progress: &progress,
        cancel: &ctx.cancel,
    };
    match st::run(name, &args, &hooks) {
        Ok(mut full) => {
            if let (Some(ex), Some(o)) = (extracted, full.as_object_mut()) {
                o.insert("extracted".into(), ex);
            }
            // Best effort: the panel is a view of the result, never a gate.
            // 3D (globe) runs have no section/claims the panel can draw yet.
            if full.get("device_kind").and_then(Value::as_str) != Some("3d") {
                let _ = ctx.store.host_call(
                    SHOW_RESULT,
                    &json!({"tool": name, "args": args, "result": full}),
                    &Value::Null,
                );
            }
            Ok(ToolOutput::structured(st::compact_for_agent(name, full)))
        }
        // Tool-level isError (like every native tool), so the agent reads why.
        Err(st::ToolError::Invalid(m)) => {
            Ok(ToolOutput::error(format!("{name}: invalid arguments: {m}")))
        }
        Err(st::ToolError::Cancelled) => Ok(ToolOutput::error(format!("{name} cancelled"))),
        Err(e @ st::ToolError::Solver(_)) => Ok(ToolOutput::error(format!("{name}: {e}"))),
    }
}

/// One `ToolDef` per solver tool.
pub fn tools() -> Vec<ToolDef> {
    st::descriptors()
        .into_iter()
        .filter_map(|d| {
            let name = d["name"].as_str()?.to_string();
            let n2 = name.clone();
            let mut input_schema = d["inputSchema"].clone();
            if DOC_TOOLS.contains(&name.as_str()) {
                add_document_props(&name, &mut input_schema);
            }
            Some(ToolDef {
                long_running: st::LONG_RUNNING.contains(&name.as_str()),
                description: d["description"].as_str().unwrap_or("").to_string(),
                input_schema,
                output_schema: None,
                handler: Arc::new(move |ctx, args| run(ctx, &n2, args)),
                name,
            })
        })
        .collect()
}

/// Add `document_id` + `extract` to a sim tool's schema and drop `spec`
/// from `required` (one of the two is needed; the handler enforces it).
fn add_document_props(name: &str, schema: &mut Value) {
    if let Some(req) = schema.get_mut("required").and_then(Value::as_array_mut) {
        req.retain(|r| r != "spec");
    }
    let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(d) = props
        .get_mut("spec")
        .and_then(|s| s.get_mut("description"))
    {
        let t = format!("{} Pass this OR `document_id` + `extract`.", d.as_str().unwrap_or(""));
        *d = Value::String(t.trim().to_string());
    }
    let shield = name == "simulate_neutron_shield";
    props.insert(
        "document_id".into(),
        json!({"type": "string", "description": if shield {
            "Session document to extract the shield from (instead of `spec`): the layer stack is measured from the document's own evaluated solids. Requires `extract`."
        } else {
            "Session document to extract the device from (instead of `spec`): the chamber and grid are measured from the document's own evaluated solids, so the simulation runs on the geometry the drawings show. Requires `extract`."
        }}),
    );
    let extract = if shield {
        let spec = &props["spec"]["properties"];
        json!({
            "type": "object", "required": ["shells", "source", "detectors"],
            "description": "Which parts of `document_id` are the shield. Each shell part must be a concentric spherical shell (sphere minus a smaller concentric sphere, or a solid ball); thicknesses are measured from the BRep. Fail-closed: a cylindrical/box shield, an off-centre or overlapping shell, or a material outside the library errors naming the part.",
            "properties": {
                "shells": {"type": "array", "minItems": 1, "items": {"type": "object", "required": ["part"], "properties": {
                    "part": {"type": "string", "description": "Shell part (name or part id)."},
                    "material": {"type": "string", "description": "Library material (hdpe, paraffin, borated-hdpe-5, water, lead, concrete, air, void). Omitted = the part's document material."}
                }}},
                "fill_material": {"type": "string", "description": "Material for the core, the gaps between shells and the world beyond the last shell. Default \"air\"."},
                "source": spec["source"].clone(),
                "detectors": spec["detectors"].clone(),
                "world_radius_mm": {"type": "number", "description": "Outer radius of the modelled world, mm. Default: 10% past the outermost detector shell."},
                "center": {"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3, "description": "Source point [x, y, z] mm; every shell must be centred on it. Default: the shells' common centre."},
                "run": spec["run"].clone()
            }
        })
    } else {
        json!({
            "type": "object", "required": ["chamber", "electrodes"],
            "description": "Which parts of `document_id` are the device. Geometry is measured from the BRep (literals); potentials and ampere-turns are NAMED DOCUMENT PARAMETERS (loon `defparam`), so scrubbing them in the document re-prices the run. Fail-closed: a ring off the chamber axis, crossing or trimmed wires, a stalk fused into the grid part, a non-cylindrical or ported chamber, or an undeclared parameter errors naming the part.",
            "properties": {
                "chamber": {"type": "string", "description": "Chamber part (name or part id), modelled as a closed can: outer cylinder minus a coaxial inner cylinder."},
                "wall_potential": {"type": "string", "description": "Document parameter holding the wall potential, V. Omitted = grounded."},
                "electrodes": {"type": "array", "minItems": 1, "items": {"type": "object", "required": ["part", "potential"], "properties": {
                    "part": {"type": "string", "description": "Electrode part (name or part id) holding only complete, separate toroidal wire rings coaxial with the chamber."},
                    "potential": {"type": "string", "description": "Document parameter holding the potential, V."},
                    "ampere_turns": {"type": "string", "description": "Document parameter holding the ring current, ampere-turns (+ = CCW viewed from +axis)."}
                }}}
            }
        })
    };
    props.insert("extract".into(), extract);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::HeadlessStore;

    #[test]
    fn solver_tools_are_native_and_long_running() {
        let ts = tools();
        let names: Vec<&str> = ts.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, st::TOOL_NAMES);
        for t in &ts {
            // The sims match the shared long-running list; compare is instant.
            assert_eq!(
                t.long_running,
                super::super::LONG_RUNNING_TOOLS.contains(&t.name.as_str()),
                "{}",
                t.name
            );
        }
    }

    #[test]
    fn agent_result_is_compacted() {
        let ctx = ToolCtx::basic(Arc::new(HeadlessStore::in_memory()));
        let spec = json!({
            "chamber_radius_mm": 76.0, "chamber_half_height_mm": 76.0,
            "rings": [{"ring_radius_mm": 19.0, "z_mm": 0.0, "wire_radius_mm": 1.0, "potential_v": -30000.0}]
        });
        let out = run(
            &ctx,
            "simulate_charged_particles",
            json!({"spec": spec, "options": {"nr": 31, "nz": 61, "particles": 4, "max_passes": 3}, "section": true}),
        )
        .unwrap();
        assert!(!out.is_error);
        let s = out.structured.unwrap();
        assert!(s["section"].get("tracks").is_none());
        assert!(s["section"]["omitted"].as_str().unwrap().contains("tracks"));
        assert!(s["claim_set"]["claims"].is_array());
        // A bad spec is a tool-level error the agent can read, not a crash.
        let bad = run(&ctx, "simulate_charged_particles", json!({"spec": {}})).unwrap();
        assert!(bad.is_error);
        assert!(bad.content[0]["text"]
            .as_str()
            .unwrap()
            .contains("invalid arguments"));
    }

    /// #1187's document path, served natively: the device and shield are
    /// measured from a session document (the vcad-eval fusor fixture, cut
    /// down), the caller's parameters override the document's, and the
    /// result echoes the extraction for audit.
    #[test]
    fn document_id_extracts_the_device_and_shield() {
        let ctx = ToolCtx::basic(Arc::new(HeadlessStore::in_memory()));
        let rings: Vec<String> = (0..4)
            .map(|k| {
                let th = std::f64::consts::PI * (k as f64 + 1.0) / 5.0;
                format!(
                    "[translate 0.0 0.0 [* grid_r {:.17}] [torus [* grid_r {:.17}] wire_r]]",
                    th.cos(),
                    th.sin()
                )
            })
            .collect();
        let src = format!(
            "[defparam cathode_v -30000.0]\n[defparam src_rate 25000.0]\n\
             [defparam grid_r 19.05]\n[defparam wire_r 1.0]\n\
             [root-named [translate 0.0 0.0 -79.2 [difference [translate 0.0 0.0 3.0 \
             [cylinder 76.2 152.4]] [cylinder 79.2 158.4]]] \"steel\" \"chamber\"]\n\
             [root-named [union {}] \"tungsten\" \"grid\"]\n\
             [root-named [difference [sphere 150.0] [sphere 300.0]] \"hdpe\" \"shield\"]\n",
            rings.join(" ")
        );
        let doc = vcad_loon::eval_vcad(&src, None).expect("fixture evaluates");
        let id = ctx.store.create(doc, None).unwrap();
        let extract = json!({
            "chamber": "chamber",
            "electrodes": [{"part": "grid", "potential": "cathode_v"}]
        });
        let out = run(
            &ctx,
            "simulate_charged_particles",
            json!({
                "document_id": id.to_string(), "extract": extract,
                "parameters": {"cathode_v": -20000.0},
                "options": {"nr": 31, "nz": 61, "particles": 4, "max_passes": 3},
                "section": false
            }),
        )
        .unwrap();
        assert!(!out.is_error, "{:?}", out.content);
        let s = out.structured.unwrap();
        assert_eq!(s["extracted"]["spec"]["rings"].as_array().unwrap().len(), 4);
        assert_eq!(s["extracted"]["parameters"]["cathode_v"], json!(-20000.0));
        assert_eq!(s["extracted"]["document_id"], json!(id.to_string()));

        let shield = run(
            &ctx,
            "simulate_neutron_shield",
            json!({
                "document_id": id.to_string(),
                "extract": {
                    "shells": [{"part": "shield"}],
                    "source": {"rate_n_per_s": "src_rate", "energy_ev": 2.45e6},
                    "detectors": [{"label": "operator", "radius_mm": 1000.0}],
                    "run": {"histories_per_batch": 500, "batches": 2, "seed": 3}
                }
            }),
        )
        .unwrap();
        assert!(!shield.is_error, "{:?}", shield.content);
        let s = shield.structured.unwrap();
        assert_eq!(s["extracted"]["parameters"]["src_rate"], json!(25000.0));
        assert!(s["extracted"]["layer_parts"]
            .as_array()
            .unwrap()
            .contains(&json!("shield")));

        // Fail closed with a readable reason.
        for (args, why) in [
            (json!({"document_id": id.to_string()}), "requires `extract`"),
            (
                json!({"document_id": id.to_string(), "extract": extract, "spec": {}}),
                "not both",
            ),
            (
                json!({"document_id": id.to_string(), "extract": {"chamber": "nope", "electrodes": [{"part": "grid", "potential": "cathode_v"}]}}),
                "nope",
            ),
        ] {
            let r = run(&ctx, "simulate_charged_particles", args).unwrap();
            assert!(r.is_error);
            let t = r.content[0]["text"].as_str().unwrap();
            assert!(t.contains(why), "{t}");
        }
        // The schema advertises the path and no longer requires `spec`.
        let t = tools()
            .into_iter()
            .find(|t| t.name == "simulate_neutron_shield")
            .unwrap();
        assert!(t.input_schema["properties"]["extract"].is_object());
        assert!(!t.input_schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("spec")));
    }
}
