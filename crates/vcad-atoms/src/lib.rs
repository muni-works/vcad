//! # vcad-atoms — M4 "agents that touch atoms" foundations
//!
//! Long-running, cancellable, fail-closed tools for the MCP-native vcad
//! (`docs/launch/mcp-native.md`, milestone M4):
//!
//! - [`fab::fab_print`] — slice → verify G-code → stream to a **dry-run** printer.
//! - [`fab::fab_mill`] — CAM → stock-sim verify vs the model → stream to a
//!   **dry-run** mill.
//! - [`optimize::optimize_parameters`] — gradient descent over named document
//!   parameters through the differentiable seam, FD-checked, emitting a
//!   per-face sensitivity field for the viewport heatmap.
//!
//! Each runs as a [`task`] (progress + cancel + terminal status), the
//! transport-free half of MCP Tasks. `crates/vcad-mcp` registers
//! [`tool_descriptors`] and calls [`spawn_tool`]; no dependency points the
//! other way, so the MCP core can depend on this crate.
//!
//! There is no hardware backend here, by construction — see [`fab`].

#![warn(missing_docs)]

pub mod fab;
pub mod optimize;
pub mod task;

use serde_json::{json, Value};
use vcad_ir::Document;

use crate::fab::DryRunMachine;
use crate::task::{JobError, TaskHandle};

/// MCP tool descriptors (name, description, input schema) for every tool in
/// this crate. All are task-capable (`execution.taskSupport = "required"`).
pub fn tool_descriptors() -> Vec<Value> {
    let task = json!({ "taskSupport": "required" });
    vec![
        json!({
            "name": "fab_print",
            "description": "Slice the open document for a 3D printer (default Bambu P1S), verify the G-code against fail-closed gates (fits bed, motion within bed, Z within part, extrudes), then stream it to a DRY-RUN printer. Long-running task with progress and cancel. Never sends to real hardware.",
            "execution": task,
            "inputSchema": { "type": "object", "properties": {
                "printer": { "type": "string", "enum": ["bambu_p1s", "bambu_x1c", "bambu_a1", "bambu_a1_mini", "prusa_mk4"] },
                "layerHeight": { "type": "number" }, "infill": { "type": "number" },
                "walls": { "type": "integer" }, "printTemp": { "type": "number" }, "bedTemp": { "type": "number" }
            }}
        }),
        json!({
            "name": "fab_mill",
            "description": "CAM an outside profile for a Carvera-class 3-axis mill, simulate stock removal and grade the cut against the model (no gouge, no leftover), check motion stays in the stock and off the spoilboard, then stream to a DRY-RUN mill. Long-running task; any failed gate fails the task with the full gate report. Never sends to real hardware.",
            "execution": task,
            "inputSchema": { "type": "object", "properties": {
                "toolDiameter": { "type": "number" }, "stepdown": { "type": "number" },
                "feed": { "type": "number" }, "plunge": { "type": "number" }, "rpm": { "type": "number" },
                "margin": { "type": "number" }, "stockDepth": { "type": "number" },
                "tolerance": { "type": "number" }, "allowance": { "type": "number" }
            }}
        }),
        json!({
            "name": "optimize_parameters",
            "description": "Minimize (or maximize) a quantity — mass, volume, bbox extent, centroid — over named document parameters with bounds and QoI constraints (e.g. bbox_z >= 20), using exact seam gradients. Result includes the optimum, iteration history, a central-FD gradient check (fails closed on disagreement) and per-face normal-velocity sensitivities for a heatmap. Stiffness/stress goals are refused until the FEA adjoint is wired.",
            "execution": task,
            "inputSchema": { "type": "object", "required": ["parameters"], "properties": {
                "parameters": { "type": "array", "items": { "type": "object", "required": ["name", "min", "max"],
                    "properties": { "name": {"type": "string"}, "min": {"type": "number"}, "max": {"type": "number"} } } },
                "objective": { "type": "object", "properties": {
                    "qoi": { "type": "string" }, "sense": { "type": "string", "enum": ["min", "max"] }, "part": { "type": "integer" } } },
                "constraints": { "type": "array", "items": { "type": "object", "required": ["qoi", "value"], "properties": {
                    "qoi": {"type": "string"}, "op": {"type": "string", "enum": [">=", "<="]},
                    "value": {"type": "number"}, "part": {"type": "integer"} } } },
                "density": { "type": "number" }, "maxIterations": { "type": "integer" }, "tolerance": { "type": "number" }
            }}
        }),
    ]
}

type BoxedJob = Box<dyn FnOnce(&task::TaskCtx) -> Result<Value, JobError> + Send>;

/// Start tool `name` on `doc` as a background task. Unknown names return
/// `None`, so a caller can fall through to its other registries.
pub fn spawn_tool(name: &str, doc: Document, args: Value) -> Option<TaskHandle> {
    let job: BoxedJob = match name {
        "fab_print" => Box::new(move |ctx| {
            fab::fab_print(&doc, &args, &DryRunMachine::new("Bambu P1S (dry run)"), ctx)
        }),
        "fab_mill" => Box::new(move |ctx| {
            fab::fab_mill(
                &doc,
                &args,
                &DryRunMachine::new("Carvera Air (dry run)"),
                ctx,
            )
        }),
        "optimize_parameters" => {
            Box::new(move |ctx| optimize::optimize_parameters(&doc, &args, ctx))
        }
        _ => return None,
    };
    Some(task::spawn(job))
}
