//! Halluacinate engine — owns the Lua VM, runs on a dedicated thread.
//!
//! The engine receives `ScriptRequest` messages via an mpsc channel and
//! dispatches them to the sandboxed Lua state. Each script gets its own
//! `_ENV` table for namespace isolation.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use mlua::Function;
use mlua::Lua;
use mlua::RegistryKey;
use mlua::Value;
use serde_json::Value as JsonValue;
use tokio::sync::mpsc;
use tracing;

use crate::api::ScriptRegistrations;
use crate::api::SessionInfo;
use crate::api::{self};
use crate::discovery;
use crate::handle::HalluacinateHandle;
use crate::handle::HookResult;
use crate::handle::ReloadResult;
use crate::handle::ScriptRequest;
use crate::handle::ScriptTool;
use crate::handle::StatusLineSpan;
use crate::handle::ToolResult;
use crate::sandbox::Deadline;
use crate::sandbox::{self};

const DEFAULT_STATUSLINE_SCRIPT_NAME: &str = "__chaos_default_statusline__";
const DEFAULT_STATUSLINE_SCRIPT: &str = include_str!("../scripts/default_statusline.lua");

/// Default per-invocation execution deadline.
const INVOCATION_DEADLINE: Duration = Duration::from_secs(10);

/// Generous deadline for initial script loading (multiple scripts).
const LOAD_DEADLINE: Duration = Duration::from_secs(30);

/// Channel buffer size.
const CHANNEL_BUFFER: usize = 64;

/// The Lua engine. Not `Send` — lives on a dedicated OS thread.
pub struct HalluacinateEngine {
    lua: Lua,
    /// Shared deadline handle — reset before each Lua call.
    deadline: Deadline,
    /// Hook event name → list of handler registry keys.
    hooks: HashMap<String, Vec<RegistryKey>>,
    /// Tool name → (spec, handler key).
    tools: HashMap<String, (ScriptTool, RegistryKey)>,
    /// Optional status-line renderer function key.
    statusline_renderer: Option<RegistryKey>,
    /// Session info shared with scripts.
    info: Arc<SessionInfo>,
    /// Working directory (for script discovery).
    cwd: PathBuf,
    /// Override for the user-layer scripts directory (see `SessionInfo`).
    user_scripts_dir: Option<PathBuf>,
}

impl HalluacinateEngine {
    /// Create a new engine and load scripts from the standard directories.
    pub fn new(info: SessionInfo) -> anyhow::Result<Self> {
        let lua = Lua::new();
        let deadline = sandbox::apply(&lua)?;

        let cwd = PathBuf::from(&info.cwd);
        let user_scripts_dir = info.user_scripts_dir.clone();
        let info = Arc::new(info);

        let mut engine = Self {
            lua,
            deadline,
            hooks: HashMap::new(),
            tools: HashMap::new(),
            statusline_renderer: None,
            info,
            cwd,
            user_scripts_dir,
        };

        engine.load_scripts();
        Ok(engine)
    }

    /// Discover and load all scripts, collecting their registrations.
    fn load_scripts(&mut self) {
        self.deadline.reset(LOAD_DEADLINE);
        self.load_default_statusline_script();
        let paths = discovery::discover_scripts(&self.cwd, self.user_scripts_dir.as_deref());
        for path in &paths {
            if let Err(e) = self.load_script(path) {
                tracing::warn!(
                    script = %path.display(),
                    "failed to load lua script: {e}"
                );
            }
        }
        if !paths.is_empty() {
            tracing::info!(count = paths.len(), "halluacinate: loaded lua scripts");
        }
    }

    fn load_default_statusline_script(&mut self) {
        if let Err(e) =
            self.load_script_source(DEFAULT_STATUSLINE_SCRIPT_NAME, DEFAULT_STATUSLINE_SCRIPT)
        {
            tracing::warn!("failed to load built-in lua statusline script: {e}");
        }
    }

    /// Load a single script file into its own sandboxed environment.
    fn load_script(&mut self, path: &Path) -> anyhow::Result<()> {
        let source = std::fs::read_to_string(path)?;
        let script_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        self.load_script_source(script_name, &source)
    }

    /// Load a single script source into its own sandboxed environment.
    fn load_script_source(&mut self, script_name: &str, source: &str) -> anyhow::Result<()> {
        // Each script gets its own _ENV + chaos table (plain table, not userdata).
        let env = sandbox::new_script_env(&self.lua)?;
        let regs = Arc::new(Mutex::new(ScriptRegistrations::new()));

        let chaos_table = api::create_chaos_table(&self.lua, &self.info, &regs, script_name)?;
        env.set("chaos", chaos_table)?;

        // Execute the script in its sandboxed env.
        self.lua
            .load(source)
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
        if let Some(key) = regs.statusline_renderer.take() {
            self.statusline_renderer = Some(key);
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
    fn list_tools(&self) -> Vec<ScriptTool> {
        self.tools.values().map(|(spec, _)| spec.clone()).collect()
    }

    /// Render a status line using the registered Lua function.
    fn render_statusline(&self, ctx: &JsonValue) -> Option<Vec<StatusLineSpan>> {
        let key = self.statusline_renderer.as_ref()?;
        self.deadline.reset(INVOCATION_DEADLINE);
        let func = self.lua.registry_value::<Function>(key).ok()?;
        let lua_ctx = api::lua_value_from_json(&self.lua, ctx).ok()?;
        let result = func.call::<Value>(lua_ctx).ok()?;
        parse_statusline_result(result)
    }

    /// Reload all scripts from disk.
    fn reload(&mut self) -> ReloadResult {
        self.hooks.clear();
        self.tools.clear();
        self.statusline_renderer = None;

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

        let paths = discovery::discover_scripts(&self.cwd, self.user_scripts_dir.as_deref());
        let mut errors = Vec::new();

        self.deadline.reset(LOAD_DEADLINE);
        self.load_default_statusline_script();
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
    pub fn run(mut self, mut rx: mpsc::Receiver<ScriptRequest>) {
        while let Some(req) = rx.blocking_recv() {
            match req {
                ScriptRequest::DispatchHook {
                    event,
                    payload,
                    reply,
                } => {
                    let result = self.dispatch_hook(&event, &payload);
                    let _ = reply.send(result);
                }
                ScriptRequest::CallTool { name, args, reply } => {
                    let result = self.call_tool(&name, &args);
                    let _ = reply.send(result);
                }
                ScriptRequest::ListTools { reply } => {
                    let _ = reply.send(self.list_tools());
                }
                ScriptRequest::Reload { reply } => {
                    let result = self.reload();
                    let _ = reply.send(result);
                }
                ScriptRequest::RenderStatusLine { ctx, reply } => {
                    let result = self.render_statusline(&ctx);
                    let _ = reply.send(result);
                }
                ScriptRequest::Shutdown => {
                    tracing::info!("halluacinate engine shutting down");
                    break;
                }
            }
        }
    }
}

fn parse_statusline_result(value: Value) -> Option<Vec<StatusLineSpan>> {
    let table = match value {
        Value::String(text) => {
            return Some(vec![StatusLineSpan {
                text: text.to_string_lossy(),
                color: None,
                bold: false,
                line_break: false,
            }]);
        }
        Value::Table(t) => t,
        _ => return None,
    };
    let mut spans = Vec::new();
    for pair in table.sequence_values::<Value>() {
        let Ok(Value::Table(entry)) = pair else {
            continue;
        };
        let text: String = entry.get("text").unwrap_or_default();
        let color: Option<String> = entry.get("color").ok().and_then(|v: Value| match v {
            Value::String(s) => Some(s.to_string_lossy()),
            _ => None,
        });
        let bold: bool = entry.get("bold").unwrap_or(false);
        let line_break: bool = entry.get("line_break").unwrap_or(false);
        spans.push(StatusLineSpan {
            text,
            color,
            bold,
            line_break,
        });
    }
    Some(spans)
}

/// Spawn the engine on a blocking thread and return an async handle.
pub fn spawn(info: SessionInfo) -> anyhow::Result<HalluacinateHandle> {
    let (tx, rx) = mpsc::channel(CHANNEL_BUFFER);
    let engine = HalluacinateEngine::new(info)?;

    std::thread::Builder::new()
        .name("halluacinate".to_owned())
        .spawn(move || engine.run(rx))?;

    Ok(HalluacinateHandle::new(tx))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    #[test]
    fn parses_plain_string_statusline_result() {
        let lua = Lua::new();
        let value = Value::String(lua.create_string("ready").unwrap());

        let spans = parse_statusline_result(value).unwrap();

        assert_eq!(
            spans,
            vec![StatusLineSpan {
                text: "ready".to_string(),
                color: None,
                bold: false,
                line_break: false,
            }]
        );
    }

    #[tokio::test]
    async fn statusline_renderer_receives_context_and_returns_spans() {
        let temp = tempfile::tempdir().unwrap();
        let scripts_dir = temp.path().join(".chaos").join("scripts");
        fs::create_dir_all(&scripts_dir).unwrap();
        fs::write(
            scripts_dir.join("status.lua"),
            r#"
chaos.statusline(function(ctx)
  return {
    { text = ctx.model, color = "green", bold = true },
    { text = " " .. ctx.cwd },
  }
end)
"#,
        )
        .unwrap();

        let handle = crate::spawn(SessionInfo {
            session_id: "session".to_string(),
            cwd: temp.path().to_string_lossy().to_string(),
            provider: "test".to_string(),
            user_scripts_dir: Some(temp.path().join("no_user_scripts")),
        })
        .unwrap();

        let spans = handle
            .render_statusline(json!({
                "model": "gpt-test",
                "cwd": "/work/repo",
            }))
            .await
            .unwrap();
        handle.shutdown().await;

        assert_eq!(
            spans,
            vec![
                StatusLineSpan {
                    text: "gpt-test".to_string(),
                    color: Some("green".to_string()),
                    bold: true,
                    line_break: false,
                },
                StatusLineSpan {
                    text: " /work/repo".to_string(),
                    color: None,
                    bold: false,
                    line_break: false,
                },
            ]
        );
    }

    #[tokio::test]
    async fn default_statusline_renderer_is_available_without_user_scripts() {
        let temp = tempfile::tempdir().unwrap();
        let handle = crate::spawn(SessionInfo {
            session_id: "session".to_string(),
            cwd: temp.path().to_string_lossy().to_string(),
            provider: "test".to_string(),
            user_scripts_dir: Some(temp.path().join("no_user_scripts")),
        })
        .unwrap();

        let spans = handle
            .render_statusline(json!({
                "model": "gpt-test",
                "reasoning_effort": "high",
                "cwd_display": "~/repo",
                "context": {
                    "remaining_pct": 87,
                },
            }))
            .await
            .unwrap();
        handle.shutdown().await;

        let text = spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert_eq!(text, "HUD · HP [===-] 87% · WPN gpt-test high · MAP ~/repo");
    }
}
