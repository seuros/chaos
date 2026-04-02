//! Script engine trait — the contract between the hallucinate runtime
//! and any script backend (Lua, WASM, or whatever comes next).

use serde_json::Value as JsonValue;
use std::path::Path;

use crate::handle::HookResult;
use crate::handle::ScriptTool;
use crate::handle::ToolResult;

/// Registrations collected from a loaded script.
#[derive(Debug, Default)]
pub struct ScriptRegistrations {
    pub hooks: Vec<(String, HookHandler)>,
    pub tools: Vec<ScriptTool>,
}

/// Opaque hook handler — engine-specific, lives behind a Box.
pub type HookHandler = Box<dyn std::any::Any + Send>;

/// A script engine backend. Implementations are not Send — they live
/// on a dedicated OS thread and are driven by the message loop in vm.rs.
pub trait ScriptEngine {
    /// Load a script file and return its registrations (hooks + tools).
    fn load_script(&mut self, path: &Path) -> anyhow::Result<()>;

    /// Dispatch a hook event to all registered handlers.
    fn dispatch_hook(&self, event: &str, payload: &JsonValue) -> HookResult;

    /// Call a script-defined tool by name.
    fn call_tool(&self, name: &str, args: &JsonValue) -> ToolResult;

    /// List all tools registered by loaded scripts.
    fn list_tools(&self) -> Vec<ScriptTool>;

    /// Clear all loaded scripts and registrations, then reload from disk.
    fn reload(&mut self, cwd: &Path) -> crate::handle::ReloadResult;

    /// File extension this engine handles (e.g., "lua", "wasm").
    fn extension(&self) -> &str;
}
