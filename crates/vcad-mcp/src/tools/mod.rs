//! Tool plumbing: definitions, call context, results.
//!
//! Native tools declare their arguments as a `#[derive(ToolSchema)]` enum
//! variant and their structured result as a `#[derive(OutputSchema)]` struct
//! (both from `vcad-tool-derive`), so `inputSchema` and `outputSchema` come
//! from Rust types rather than hand-written JSON.

pub mod atoms;
pub mod core;
pub mod docs;
pub mod engine;
pub mod host;
pub mod instrument;
pub mod loon_carry;
pub mod proof;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::store::DocumentStore;

/// One `tools/list` entry produced by `#[derive(ToolSchema)]`. The derive
/// macro constructs `crate::ToolSchemaEntry`, so the type lives here.
#[derive(Debug, Clone)]
pub struct ToolSchemaEntry {
    /// snake_case tool name.
    pub name: String,
    /// Description from the variant's doc comment.
    pub description: String,
    /// Category from `#[tool(category = "…")]`.
    pub category: String,
    /// Optional `#[tool(ai_hint = "…")]`.
    pub ai_hint: Option<String>,
    /// JSON Schema of the arguments.
    pub input_schema: Value,
}

/// Progress callback: `(progress, total, message)`.
pub type ProgressFn = Arc<dyn Fn(f64, Option<f64>, Option<String>) + Send + Sync>;

/// Elicitation callback: `elicitation/create` params in, client result out
/// (`{"action":"accept","content":{…}}` / `decline` / `cancel`). How it is
/// answered depends on the era: MRTR retry, a parked Task, or a live
/// server→client request on a legacy connection.
pub type ElicitFn = Arc<dyn Fn(Value) -> Value + Send + Sync>;

/// What a tool handler gets.
#[derive(Clone)]
pub struct ToolCtx {
    /// The document store.
    pub store: Arc<dyn DocumentStore>,
    /// Report progress (no-op when nobody listens).
    pub progress: ProgressFn,
    /// Ask the user something.
    pub elicit: ElicitFn,
    /// Cooperative cancel flag (set by `tasks/cancel`).
    pub cancel: Arc<AtomicBool>,
    /// The MRTR `inputResponses` the client sent with this call (`Null` when
    /// none). Host tools that answer elicitations natively in the app (the
    /// viewport pick, the native sheet) use a client-provided answer first.
    pub input_responses: Value,
    /// The running job's id when this call is a long-running job
    /// ([`crate::jobs`]); host tools pass it to the app as `_vcad_job`.
    pub job_id: Option<String>,
}

impl ToolCtx {
    /// A context with no-op progress, elicitations cancelled.
    pub fn basic(store: Arc<dyn DocumentStore>) -> Self {
        ToolCtx {
            store,
            progress: Arc::new(|_, _, _| {}),
            elicit: Arc::new(|_| json!({"action": "cancel"})),
            cancel: Arc::new(AtomicBool::new(false)),
            input_responses: Value::Null,
            job_id: None,
        }
    }
}

/// A tool's result.
#[derive(Debug, Clone, Default)]
pub struct ToolOutput {
    /// MCP content blocks (`{"type":"text",…}`, `{"type":"image",…}`).
    pub content: Vec<Value>,
    /// `structuredContent`, validated by the tool's `outputSchema`.
    pub structured: Option<Value>,
    /// `isError`.
    pub is_error: bool,
}

impl ToolOutput {
    /// Structured result plus its JSON as a text block (for clients that
    /// ignore `structuredContent`).
    pub fn structured(value: Value) -> Self {
        let text = serde_json::to_string_pretty(&value).unwrap_or_default();
        ToolOutput {
            content: vec![json!({"type": "text", "text": text})],
            structured: Some(value),
            is_error: false,
        }
    }

    /// A tool-level error (`isError: true`), not a protocol error.
    pub fn error(message: impl Into<String>) -> Self {
        ToolOutput {
            content: vec![json!({"type": "text", "text": message.into()})],
            structured: None,
            is_error: true,
        }
    }

    /// Wire shape (`CallToolResult`, without `_meta`).
    pub fn to_json(&self) -> Value {
        let mut out = json!({ "content": self.content });
        if let Some(s) = &self.structured {
            out["structuredContent"] = s.clone();
        }
        if self.is_error {
            out["isError"] = Value::Bool(true);
        }
        out
    }
}

/// Why a native handler did not produce an output.
#[derive(Debug, Clone)]
pub enum ToolError {
    /// Bad arguments → JSON-RPC invalid params.
    InvalidArgs(String),
    /// The call needs a feature this native port lacks: hand it to the node
    /// server unchanged (the strangler seam).
    Delegate(String),
}

/// Handler signature.
pub type ToolHandler = Arc<dyn Fn(&ToolCtx, Value) -> Result<ToolOutput, ToolError> + Send + Sync>;

/// A registered tool.
#[derive(Clone)]
pub struct ToolDef {
    /// Tool name.
    pub name: String,
    /// Description.
    pub description: String,
    /// JSON Schema of arguments.
    pub input_schema: Value,
    /// JSON Schema of `structuredContent`, when the tool returns one.
    pub output_schema: Option<Value>,
    /// Long-running: returns a Task handle to task-capable modern clients.
    pub long_running: bool,
    /// The implementation.
    pub handler: ToolHandler,
}

impl ToolDef {
    /// Build from a derived schema entry.
    pub fn from_entry(
        entry: ToolSchemaEntry,
        output_schema: Option<Value>,
        handler: ToolHandler,
    ) -> Self {
        let mut description = entry.description;
        if let Some(h) = entry.ai_hint {
            description.push_str("\n\n");
            description.push_str(&h);
        }
        ToolDef {
            name: entry.name,
            description,
            input_schema: entry.input_schema,
            output_schema,
            long_running: false,
            handler,
        }
    }

    /// `tools/list` entry.
    pub fn listing(&self) -> Value {
        let mut v = json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
        });
        if let Some(o) = &self.output_schema {
            v["outputSchema"] = o.clone();
        }
        if self.long_running {
            v["execution"] = json!({ "taskSupport": "optional" });
        }
        v
    }
}

/// Tool names (served natively OR by the node server) that earn a Task handle
/// for task-capable clients. Mirrors `LONG_RUNNING_TOOLS` in protocol-2026.ts.
pub const LONG_RUNNING_TOOLS: &[&str] = &[
    "route_nets",
    "route_diff_pair",
    "length_match_traces",
    "fab_prep",
    "fix_drc",
    "topology_optimize",
    "optimize_electrodes",
    "export_video",
    "render_sequence",
    "simulate_flow",
    "simulate_em",
    "simulate_photonics",
    "simulate_charged_particles",
    "simulate_neutron_shield",
    "simulate_lattice_gauge",
    "md_run",
    "minimize_energy",
    "homogenize_material",
    "design_material",
];

/// Every native tool, in both vcad.app and headless: the M0 core, document
/// tools, the engine's edit vocabulary, receipts, `vcad-atoms` tasks and the
/// host tools (executed by the app; headless answers or delegates). Later
/// entries replace earlier ones of the same name.
pub fn native_tools(info: core::InfoSource) -> Vec<ToolDef> {
    let mut by_name: std::collections::BTreeMap<String, ToolDef> = Default::default();
    for t in core::core_tools(info)
        .into_iter()
        .chain(docs::tools())
        .chain(engine::tools())
        .chain(proof::tools())
        .chain(atoms::tools())
        .chain(host::tools())
        .chain(instrument::instrument_tools())
    {
        by_name.insert(t.name.clone(), t);
    }
    by_name.into_values().collect()
}
