//! TUI chat panel — local build.
//!
//! The chat panel used to stream from vcad.io's hosted `/api/chat`. vcad
//! no longer operates hosted services, so the panel fails closed: a sent
//! message is answered with a pointer to the local MCP server
//! (`npx -y @vcad/mcp`), which lets any agent the user brings drive vcad.

use anyhow::Result;

use crate::app::{App, LogLevel};

/// Message shown when the user tries to chat.
pub const NOT_AVAILABLE: &str = "AI chat is not available in the local build: vcad no longer \
runs a hosted chat service. Connect your own agent with the local MCP server (npx -y @vcad/mcp).";

/// Placeholder chat state. Nothing is ever in flight.
#[derive(Debug, Default)]
pub struct ChatSession;

impl ChatSession {
    /// Always false: there is no background request in the local build.
    pub fn is_busy(&self) -> bool {
        false
    }

    /// No-op: nothing to abort.
    pub fn abort(&self) {}
}

/// The chat input already echoes the user's line; nothing to record.
pub fn push_user_message(_app: &mut App, _text: String) {}

/// Fail closed with a clear explanation.
pub fn start_chat_turn(app: &mut App) -> Result<()> {
    app.chat.assistant(NOT_AVAILABLE);
    app.log(LogLevel::Info, "chat", NOT_AVAILABLE);
    Ok(())
}

/// No-op: no background events in the local build.
pub fn drain_chat_events(_app: &mut App) {}
