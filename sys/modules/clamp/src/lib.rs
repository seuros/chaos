//! # chaos-clamp
//!
//! Claude Code subprocess module for Chaos.
//!
//! Spawns Claude Code as a headless subprocess and drives it via the
//! stream-json control protocol. Claude Code becomes an invisible
//! transport layer — it authenticates with Anthropic using the user's
//! MAX subscription, while Chaos provides all tools, controls permissions,
//! and handles the user interface.

mod protocol;
mod transport;

pub use protocol::ControlRequest;
pub use protocol::ControlResponse;
pub use protocol::Message;
pub use transport::ClampConfig;
pub use transport::ClampError;
pub use transport::ClampInfo;
pub use transport::ClampTransport;
pub use transport::HookCallbackHandler;
pub use transport::McpMessageHandler;
pub use transport::ToolPermissionHandler;

use std::sync::Mutex;

/// Cached model list from Claude Code init response.
static CACHED_MODELS: Mutex<Option<serde_json::Value>> = Mutex::new(None);

/// Store the model list from the Claude Code init response.
pub fn set_cached_models(models: serde_json::Value) {
    if let Ok(mut guard) = CACHED_MODELS.lock() {
        *guard = Some(models);
    }
}

/// Get the cached Claude Code model list.
pub fn cached_models() -> Option<serde_json::Value> {
    CACHED_MODELS.lock().ok().and_then(|g| g.clone())
}
