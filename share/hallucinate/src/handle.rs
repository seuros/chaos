//! Async handle to the Hallucinate script engine.
//!
//! `HallucinateHandle` is the cheaply-cloneable interface the kernel uses
//! to talk to the script engine thread. All communication goes through an
//! mpsc channel; responses come back via oneshot. Engine-agnostic — works
//! with Lua, WASM, or any backend that implements `ScriptEngine`.

use serde_json::Value as JsonValue;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

/// Result of dispatching a hook to Lua scripts.
#[derive(Debug, Clone)]
pub enum HookResult {
    /// All handlers succeeded (or none were registered).
    Success,
    /// A handler failed but execution should continue.
    FailedContinue(String),
    /// A handler failed and requests abort.
    FailedAbort(String),
}

/// A tool defined by a script (Lua, WASM, or any engine).
#[derive(Debug, Clone)]
pub struct ScriptTool {
    pub name: String,
    pub description: String,
    pub input_schema: JsonValue,
}

/// Result of invoking a Lua-defined tool.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
}

/// Requests sent from the async world to the engine thread.
#[derive(Debug)]
pub enum ScriptRequest {
    /// Dispatch a hook event to all registered Lua handlers.
    DispatchHook {
        event: String,
        payload: JsonValue,
        reply: oneshot::Sender<HookResult>,
    },
    /// Invoke a Lua-defined tool.
    CallTool {
        name: String,
        args: JsonValue,
        reply: oneshot::Sender<ToolResult>,
    },
    /// Return all tools registered by Lua scripts.
    ListTools {
        reply: oneshot::Sender<Vec<ScriptTool>>,
    },
    /// Reload all scripts from disk.
    Reload {
        reply: oneshot::Sender<ReloadResult>,
    },
    /// Shut down the engine thread.
    Shutdown,
}

/// Outcome of a reload operation.
#[derive(Debug, Clone)]
pub struct ReloadResult {
    pub scripts_loaded: usize,
    pub errors: Vec<String>,
}

/// Async handle to the Hallucinate engine. Clone-friendly.
#[derive(Clone)]
pub struct HallucinateHandle {
    tx: mpsc::Sender<ScriptRequest>,
}

impl HallucinateHandle {
    /// Create a new handle from a channel sender.
    pub fn new(tx: mpsc::Sender<ScriptRequest>) -> Self {
        Self { tx }
    }

    /// Dispatch a hook event. Returns `HookResult::Success` if the engine
    /// is unreachable (graceful degradation).
    pub async fn dispatch_hook(&self, event: &str, payload: JsonValue) -> HookResult {
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = ScriptRequest::DispatchHook {
            event: event.to_owned(),
            payload,
            reply: reply_tx,
        };
        if self.tx.send(req).await.is_err() {
            return HookResult::Success;
        }
        reply_rx.await.unwrap_or(HookResult::Success)
    }

    /// Call a Lua-defined tool by name.
    pub async fn call_tool(&self, name: &str, args: JsonValue) -> ToolResult {
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = ScriptRequest::CallTool {
            name: name.to_owned(),
            args,
            reply: reply_tx,
        };
        if self.tx.send(req).await.is_err() {
            return ToolResult {
                success: false,
                output: "hallucinate engine unavailable".to_owned(),
            };
        }
        reply_rx.await.unwrap_or(ToolResult {
            success: false,
            output: "hallucinate engine did not respond".to_owned(),
        })
    }

    /// Get all tools registered by Lua scripts.
    pub async fn list_tools(&self) -> Vec<ScriptTool> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = ScriptRequest::ListTools { reply: reply_tx };
        if self.tx.send(req).await.is_err() {
            return Vec::new();
        }
        reply_rx.await.unwrap_or_default()
    }

    /// Trigger a hot-reload of all scripts.
    pub async fn reload(&self) -> ReloadResult {
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = ScriptRequest::Reload { reply: reply_tx };
        if self.tx.send(req).await.is_err() {
            return ReloadResult {
                scripts_loaded: 0,
                errors: vec!["engine unavailable".to_owned()],
            };
        }
        reply_rx.await.unwrap_or(ReloadResult {
            scripts_loaded: 0,
            errors: vec!["engine did not respond".to_owned()],
        })
    }

    /// Request engine shutdown.
    pub async fn shutdown(&self) {
        let _ = self.tx.send(ScriptRequest::Shutdown).await;
    }
}
