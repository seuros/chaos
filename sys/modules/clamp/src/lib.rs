//! # chaos-clamp
//!
//! First-party model CLI subprocess transports for Chaos.
//!
//! Claude Code is driven through its bidirectional stream-JSON control
//! protocol. Antigravity currently provides a model-only, sandboxed turn
//! transport through the official `agy` CLI while its Chaos-owned tool bridge
//! remains under development.

mod antigravity;
mod protocol;
mod proxy;
mod transport;

pub use antigravity::AntigravityConfig;
pub use antigravity::AntigravityError;
pub use antigravity::AntigravityEvent;
pub use antigravity::AntigravityInit;
pub use antigravity::AntigravityResult;
pub use antigravity::AntigravityStepUpdate;
pub use antigravity::AntigravityToolAuthority;
pub use antigravity::AntigravityTransport;
pub use antigravity::AntigravityTurn;
pub use antigravity::AntigravityUsage;
pub use protocol::ControlRequest;
pub use protocol::ControlResponse;
pub use protocol::Message;
pub use protocol::Usage;
pub use proxy::FileWiretapSink;
pub use proxy::WiretapExchange;
pub use proxy::WiretapProxy;
pub use proxy::WiretapSink;
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
