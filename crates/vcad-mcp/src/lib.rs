//! vcad's MCP server core.
//!
//! One Rust core serves MCP in both eras — `2026-07-28` (stateless requests,
//! `server/discover`, MRTR, Tasks, `subscriptions/listen`) and the legacy
//! `initialize` handshake — over stdio, `127.0.0.1` HTTP, or a unix socket.
//! Documents live behind the [`store::DocumentStore`] trait: headless files in
//! `~/.vcad/mcp-sessions`, or the live vcad.app document (M1). Core tools run
//! natively; anything else proxies to the node server (`packages/mcp`).
//!
//! Design: `docs/launch/mcp-native.md`.
#![warn(missing_docs)]

pub mod cli;
pub mod protocol;
pub mod proxy;
pub mod server;
pub mod store;
pub mod tasks;
pub mod tools;
pub mod transport;

pub use server::{Connection, Outcome, Server, ServerConfig};
pub use store::{
    AppBridge, AppStore, CommitMeta, DocumentId, DocumentStore, DocumentSummary, HeadlessStore,
    StoreError, StoreEvent, StoreKind, StoreListener,
};
pub use tools::{ToolCtx, ToolDef, ToolError, ToolOutput, ToolSchemaEntry};

/// Schema for a nested type used with `#[tool(expand)]` in the derives.
pub trait SubToolSchema {
    /// JSON Schema of the type.
    fn sub_schema() -> serde_json::Value;
}

/// Helpers for tests and the proxy test hook. Not part of the stable API.
#[doc(hidden)]
pub mod test_support {
    use std::sync::Arc;

    use serde_json::json;

    use crate::tools::{ToolDef, ToolOutput};

    /// A tool only the "upstream" has: echoes its arguments.
    pub fn upstream_echo_tool() -> ToolDef {
        ToolDef {
            name: "upstream_echo".into(),
            description: "Echo arguments (proxy test upstream).".into(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
            long_running: false,
            handler: Arc::new(|_, args| {
                Ok(ToolOutput::structured(
                    json!({"echo": args, "served_by": "upstream"}),
                ))
            }),
        }
    }
}
