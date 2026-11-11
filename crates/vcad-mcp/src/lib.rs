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

pub mod protocol;
pub mod store;
pub mod tools;

pub use store::{
    AppBridge, AppStore, CommitMeta, DocumentId, DocumentStore, DocumentSummary, HeadlessStore,
    StoreError, StoreEvent, StoreKind, StoreListener,
};
pub use tools::{ToolCtx, ToolDef, ToolError, ToolOutput, ToolSchemaEntry};
