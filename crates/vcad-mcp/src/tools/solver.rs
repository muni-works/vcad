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
use vcad_solver_tools as st;

use super::{ToolCtx, ToolDef, ToolError, ToolOutput};

/// Host-internal call carrying a finished solver result to vcad.app
/// (`{tool, args, result}`); never listed.
pub const SHOW_RESULT: &str = "__show_solver_result";

fn run(ctx: &ToolCtx, name: &str, mut args: Value) -> Result<ToolOutput, ToolError> {
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
        Ok(full) => {
            // Best effort: the panel is a view of the result, never a gate.
            let _ = ctx.store.host_call(
                SHOW_RESULT,
                &json!({"tool": name, "args": args, "result": full}),
                &Value::Null,
            );
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
            Some(ToolDef {
                long_running: st::LONG_RUNNING.contains(&name.as_str()),
                description: d["description"].as_str().unwrap_or("").to_string(),
                input_schema: d["inputSchema"].clone(),
                output_schema: None,
                handler: Arc::new(move |ctx, args| run(ctx, &n2, args)),
                name,
            })
        })
        .collect()
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
}
