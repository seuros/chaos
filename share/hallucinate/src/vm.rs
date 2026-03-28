//! Hallucinate engine — owns the Lua VM, runs on a dedicated thread.
//!
//! The engine receives `LuaRequest` messages via an mpsc channel and
//! dispatches them to the sandboxed Lua state. Each script gets its own
//! `_ENV` table for namespace isolation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mlua::{Function, Lua, RegistryKey, Value};
use serde_json::Value as JsonValue;
use tokio::sync::mpsc;
use tracing;

use crate::api::{self, ScriptRegistrations, SessionInfo};
use crate::discovery;
use crate::handle::{HallucinateHandle, HookResult, LuaRequest, LuaTool, ReloadResult, ToolResult};
use crate::sandbox::{self, Deadline};

/// Default per-invocation execution deadline.
const INVOCATION_DEADLINE: Duration = Duration::from_secs(10);

/// Generous deadline for initial script loading (multiple scripts).
const LOAD_DEADLINE: Duration = Duration::from_secs(30);

/// Channel buffer size.
const CHANNEL_BUFFER: usize = 64;

/// The Lua engine. Not `Send` — lives on a dedicated OS thread.
pub struct HallucinateEngine {
    lua: Lua,
    /// Shared deadline handle — reset before each Lua call.
    deadline: Deadline,
    /// Hook event name → list of handler registry keys.
    hooks: HashMap<String, Vec<RegistryKey>>,
    /// Tool name → (spec, handler key).
    tools: HashMap<String, (LuaTool, RegistryKey)>,
    /// Session info shared with scripts.
    info: Arc<SessionInfo>,
    /// Working directory (for script discovery).
    cwd: PathBuf,
}

impl HallucinateEngine {
    /// Create a new engine and load scripts from the standard directories.
    pub fn new(info: SessionInfo) -> anyhow::Result<Self> {
        let lua = Lua::new();
        let deadline = sandbox::apply(&lua)?;

        let cwd = PathBuf::from(&info.cwd);
        let info = Arc::new(info);

        let mut engine = Self {
            lua,
            deadline,
            hooks: HashMap::new(),
            tools: HashMap::new(),
            info,
            cwd,
        };

        engine.load_scripts();
        Ok(engine)
    }

    /// Discover and load all scripts, collecting their registrations.
    fn load_scripts(&mut self) {
        self.deadline.reset(LOAD_DEADLINE);
        let paths = discovery::discover_scripts(&self.cwd);
        for path in &paths {
            if let Err(e) = self.load_script(path) {
                tracing::warn!(
                    script = %path.display(),
                    "failed to load lua script: {e}"
                );
            }
        }
        if !paths.is_empty() {
            tracing::info!(count = paths.len(), "hallucinate: loaded lua scripts");
        }
    }

    /// Load a single script file into its own sandboxed environment.
    fn load_script(&mut self, path: &Path) -> anyhow::Result<()> {
        let source = std::fs::read_to_string(path)?;
        let script_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        // Each script gets its own _ENV + chaos table (plain table, not userdata).
        let env = sandbox::new_script_env(&self.lua)?;
        let regs = Arc::new(Mutex::new(ScriptRegistrations::new()));

        let chaos_table = api::create_chaos_table(&self.lua, &self.info, &regs, script_name)?;
        env.set("chaos", chaos_table)?;

        // Execute the script in its sandboxed env.
        self.lua
            .load(&source)
            .set_name(script_name)
            .set_environment(env)
            .exec()?;

        // Transfer ownership of registrations from the script context.
        let mut regs = regs
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

        for (event, keys) in regs.hooks.drain() {
            self.hooks.entry(event).or_default().extend(keys);
        }
        for (name, (tool, key)) in regs.tools.drain() {
            self.tools.insert(name, (tool, key));
        }

        Ok(())
    }

    /// Dispatch a hook event to all registered Lua handlers.
    fn dispatch_hook(&self, event: &str, payload: &JsonValue) -> HookResult {
        let Some(handlers) = self.hooks.get(event) else {
            return HookResult::Success;
        };

        self.deadline.reset(INVOCATION_DEADLINE);

        let lua_payload = match api::lua_value_from_json(&self.lua, payload) {
            Ok(v) => v,
            Err(e) => {
                return HookResult::FailedContinue(format!(
                    "failed to convert payload to lua: {e}"
                ));
            }
        };

        for key in handlers {
            let Ok(func) = self.lua.registry_value::<Function>(key) else {
                continue;
            };
            if let Err(e) = func.call::<()>(lua_payload.clone()) {
                tracing::warn!(event, "lua hook error: {e}");
                return HookResult::FailedContinue(e.to_string());
            }
        }

        HookResult::Success
    }

    /// Call a Lua-defined tool.
    fn call_tool(&self, name: &str, args: &JsonValue) -> ToolResult {
        let Some((_spec, key)) = self.tools.get(name) else {
            return ToolResult {
                success: false,
                output: format!("no lua tool named '{name}'"),
            };
        };

        self.deadline.reset(INVOCATION_DEADLINE);

        let Ok(func) = self.lua.registry_value::<Function>(key) else {
            return ToolResult {
                success: false,
                output: format!("lua tool '{name}' handler is invalid"),
            };
        };

        let lua_args = match api::lua_value_from_json(&self.lua, args) {
            Ok(v) => v,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: format!("failed to convert args: {e}"),
                };
            }
        };

        match func.call::<Value>(lua_args) {
            Ok(result) => {
                let output = match result {
                    Value::String(s) => s.to_string_lossy(),
                    other => format!("{other:?}"),
                };
                ToolResult {
                    success: true,
                    output,
                }
            }
            Err(e) => ToolResult {
                success: false,
                output: format!("lua tool '{name}' error: {e}"),
            },
        }
    }

    /// List all tools registered by scripts.
    fn list_tools(&self) -> Vec<LuaTool> {
        self.tools.values().map(|(spec, _)| spec.clone()).collect()
    }

    /// Reload all scripts from disk.
    fn reload(&mut self) -> ReloadResult {
        self.hooks.clear();
        self.tools.clear();

        // Re-create the Lua VM for a clean slate.
        let lua = Lua::new();
        match sandbox::apply(&lua) {
            Ok(dl) => {
                self.lua = lua;
                self.deadline = dl;
            }
            Err(e) => {
                return ReloadResult {
                    scripts_loaded: 0,
                    errors: vec![format!("sandbox setup failed: {e}")],
                };
            }
        }

        let paths = discovery::discover_scripts(&self.cwd);
        let mut errors = Vec::new();

        self.deadline.reset(LOAD_DEADLINE);
        for path in &paths {
            if let Err(e) = self.load_script(path) {
                errors.push(format!("{}: {e}", path.display()));
            }
        }

        ReloadResult {
            scripts_loaded: paths.len() - errors.len(),
            errors,
        }
    }

    /// Run the engine recv loop. Blocks until shutdown.
    pub fn run(mut self, mut rx: mpsc::Receiver<LuaRequest>) {
        while let Some(req) = rx.blocking_recv() {
            match req {
                LuaRequest::DispatchHook {
                    event,
                    payload,
                    reply,
                } => {
                    let result = self.dispatch_hook(&event, &payload);
                    let _ = reply.send(result);
                }
                LuaRequest::CallTool { name, args, reply } => {
                    let result = self.call_tool(&name, &args);
                    let _ = reply.send(result);
                }
                LuaRequest::ListTools { reply } => {
                    let _ = reply.send(self.list_tools());
                }
                LuaRequest::Reload { reply } => {
                    let result = self.reload();
                    let _ = reply.send(result);
                }
                LuaRequest::Shutdown => {
                    tracing::info!("hallucinate engine shutting down");
                    break;
                }
            }
        }
    }
}

/// Spawn the engine on a blocking thread and return an async handle.
pub fn spawn(info: SessionInfo) -> anyhow::Result<HallucinateHandle> {
    let (tx, rx) = mpsc::channel(CHANNEL_BUFFER);
    let engine = HallucinateEngine::new(info)?;

    std::thread::Builder::new()
        .name("hallucinate".to_owned())
        .spawn(move || engine.run(rx))?;

    Ok(HallucinateHandle::new(tx))
}
