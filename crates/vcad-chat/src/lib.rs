//! vcad-chat — AI chat contract for vcad.
//!
//! Pure, wasm-compatible pieces of the vcad chat contract: tool schemas,
//! Anthropic tool definitions, the system prompt, and the CRUD executor.
//! There is no network client — vcad operates no hosted chat service;
//! agents drive vcad through the local MCP server (`npx -y @vcad/mcp`).
//!
//! Modules:
//! - [`schemas`]  — tool schemas sourced from [`vcad_ir::CsgOp`]
//! - [`tools`]    — Anthropic tool definitions, mirrors `toAnthropicTools`
//! - [`prompt`]   — system prompt builder, mirrors `buildSystemPrompt`
//! - [`executor`] — `execute_crud`, port of `executeCrud`
//!
//! The TS reference lives in `packages/core/src/commands/registry.ts`,
//! and `packages/app/src/lib/chat-api.ts`.

pub mod executor;
pub mod prompt;
pub mod schemas;
pub mod tools;

pub use executor::{
    apply_outcome, execute_crud, plan_crud, ApplyResult, ExecutionResult, ExecutionStatus,
    PlannedResponse, ToolOutcome,
};
pub use prompt::{
    build_system_prompt, parts_from_document, type_catalog, NodeInfo, PartInfo, SelectionInfo,
};
pub use schemas::{all_schemas, type_enum};
pub use tools::{anthropic_tools, AnthropicTool};
