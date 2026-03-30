//! Lua API surface — the `chaos` global table.
//!
//! Exposes `chaos.log.*`, `chaos.on()`, `chaos.tool()`, and read-only
//! session context to Lua scripts. This is the only bridge between the
//! Lua world and Rust internals.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mlua::{Function, Lua, LuaSerdeExt, RegistryKey, Result as LuaResult, Table, Value};
use serde_json::Value as JsonValue;
use tracing;

use crate::handle::ScriptTool;

/// Read-only session info injected into every script's `chaos` table.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub cwd: String,
    pub provider: String,
}

/// Shared mutable state collected from script registration calls.
/// The engine reads this after loading each script.
#[derive(Debug, Default)]
pub struct ScriptRegistrations {
    /// Hook event name → list of Lua function registry keys.
    pub hooks: HashMap<String, Vec<RegistryKey>>,
    /// Tool name → (spec, handler registry key).
    pub tools: HashMap<String, (ScriptTool, RegistryKey)>,
}

impl ScriptRegistrations {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Build the `chaos` table as a plain Lua table with functions and fields.
///
/// Uses plain functions (not methods) so `chaos.on(...)` dot-syntax works.
/// The colon form `chaos:on(...)` would also work but is not the documented API.
pub fn create_chaos_table(
    lua: &Lua,
    info: &Arc<SessionInfo>,
    registrations: &Arc<Mutex<ScriptRegistrations>>,
    script_name: &str,
) -> LuaResult<Table> {
    let chaos = lua.create_table()?;

    // Read-only session fields.
    chaos.set("session_id", info.session_id.as_str())?;
    chaos.set("cwd", info.cwd.as_str())?;
    chaos.set("provider", info.provider.as_str())?;

    // chaos.log sub-table.
    chaos.set("log", create_log_table(lua, script_name)?)?;

    // chaos.on(event_name, handler_fn)
    let regs = registrations.clone();
    chaos.set(
        "on",
        lua.create_function(move |lua, (event, handler): (String, Function)| {
            let key = lua.create_registry_value(handler)?;
            let mut guard = regs
                .lock()
                .map_err(|e| mlua::Error::runtime(format!("lock poisoned: {e}")))?;
            guard.hooks.entry(event).or_default().push(key);
            Ok(())
        })?,
    )?;

    // chaos.tool({ name, description, input_schema, handler })
    let regs = registrations.clone();
    chaos.set(
        "tool",
        lua.create_function(move |lua, spec: Table| {
            let name: String = spec.get("name")?;
            let description: String = spec.get("description")?;
            let input_schema: Value = spec.get("input_schema")?;
            let handler: Function = spec.get("handler")?;

            let schema_json = json_from_lua_value(lua, &input_schema)?;
            let handler_key = lua.create_registry_value(handler)?;

            let tool = ScriptTool {
                name: name.clone(),
                description,
                input_schema: schema_json,
            };

            let mut guard = regs
                .lock()
                .map_err(|e| mlua::Error::runtime(format!("lock poisoned: {e}")))?;
            guard.tools.insert(name, (tool, handler_key));
            Ok(())
        })?,
    )?;

    Ok(chaos)
}

/// Create the `chaos.log` sub-table that bridges to `tracing`.
pub fn create_log_table(lua: &Lua, script_name: &str) -> LuaResult<Table> {
    let log_table = lua.create_table()?;
    let name = script_name.to_owned();

    let name_info = name.clone();
    log_table.set(
        "info",
        lua.create_function(move |_lua, msg: String| {
            tracing::info!(script = %name_info, "{msg}");
            Ok(())
        })?,
    )?;

    let name_warn = name.clone();
    log_table.set(
        "warn",
        lua.create_function(move |_lua, msg: String| {
            tracing::warn!(script = %name_warn, "{msg}");
            Ok(())
        })?,
    )?;

    let name_debug = name;
    log_table.set(
        "debug",
        lua.create_function(move |_lua, msg: String| {
            tracing::debug!(script = %name_debug, "{msg}");
            Ok(())
        })?,
    )?;

    Ok(log_table)
}

/// Convert a Lua value to serde_json::Value (best effort).
fn json_from_lua_value(lua: &Lua, value: &Value) -> LuaResult<JsonValue> {
    let json: JsonValue = lua.from_value(value.clone())?;
    Ok(json)
}

/// Convert serde_json::Value to a Lua value.
pub fn lua_value_from_json(lua: &Lua, json: &JsonValue) -> LuaResult<Value> {
    lua.to_value(json)
}
