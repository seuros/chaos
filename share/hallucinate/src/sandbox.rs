//! Lua sandbox — strips dangerous globals, enforces resource limits.
//!
//! The sandbox removes filesystem, network, and debug access from Lua.
//! Scripts get `string`, `table`, `math`, `utf8`, `coroutine`, and the
//! safe builtins. Nothing else.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use mlua::{HookTriggers, Lua, Result as LuaResult, VmState};

/// Maximum memory a Lua VM may allocate (8 MiB).
const MEMORY_LIMIT: usize = 8 * 1024 * 1024;

/// Check the deadline every N VM instructions.
const INSTRUCTION_INTERVAL: u32 = 100_000;

/// Globals that get nuked from orbit.
const DANGEROUS_GLOBALS: &[&str] = &[
    "os",
    "io",
    "debug",
    "package",
    "loadfile",
    "dofile",
    "collectgarbage",
    "require",
    "load", // can load arbitrary bytecode
];

/// Shared deadline that the engine resets before each Lua invocation.
#[derive(Clone)]
pub struct Deadline {
    /// Epoch instant (set once at construction).
    epoch: Instant,
    /// Deadline as millis since epoch. The instruction hook reads this.
    deadline_ms: Arc<AtomicU64>,
    /// Set to true when the deadline is exceeded.
    pub killed: Arc<AtomicBool>,
}

impl Deadline {
    fn new() -> Self {
        Self {
            epoch: Instant::now(),
            deadline_ms: Arc::new(AtomicU64::new(u64::MAX)),
            killed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Reset the deadline to `duration` from now.
    pub fn reset(&self, duration: Duration) {
        let now_ms = self.epoch.elapsed().as_millis() as u64;
        let deadline = now_ms.saturating_add(duration.as_millis() as u64);
        self.deadline_ms.store(deadline, Ordering::Relaxed);
        self.killed.store(false, Ordering::Relaxed);
    }

    /// Check if the deadline has passed.
    fn is_expired(&self) -> bool {
        let now_ms = self.epoch.elapsed().as_millis() as u64;
        now_ms >= self.deadline_ms.load(Ordering::Relaxed)
    }
}

/// Strip dangerous globals and enforce resource limits on a Lua VM.
/// Returns a `Deadline` handle the engine uses to reset the timer
/// before each Lua call.
pub fn apply(lua: &Lua) -> LuaResult<Deadline> {
    strip_globals(lua)?;
    set_memory_limit(lua)?;
    let deadline = set_instruction_limit(lua)?;
    Ok(deadline)
}

/// Remove all dangerous globals from the Lua state.
fn strip_globals(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();
    for name in DANGEROUS_GLOBALS {
        globals.raw_set(*name, mlua::Value::Nil)?;
    }
    Ok(())
}

/// Cap memory allocation.
fn set_memory_limit(lua: &Lua) -> LuaResult<()> {
    lua.set_memory_limit(MEMORY_LIMIT)?;
    Ok(())
}

/// Install an instruction-count hook. The hook checks a shared deadline
/// that the engine resets before each call.
fn set_instruction_limit(lua: &Lua) -> LuaResult<Deadline> {
    let deadline = Deadline::new();
    let dl = deadline.clone();

    lua.set_hook(
        HookTriggers::new().every_nth_instruction(INSTRUCTION_INTERVAL),
        move |_lua, _debug| {
            if dl.is_expired() {
                dl.killed.store(true, Ordering::Relaxed);
                Err(mlua::Error::runtime("script exceeded execution deadline"))
            } else {
                Ok(VmState::Continue)
            }
        },
    )?;

    Ok(deadline)
}

/// Create a sandboxed environment table for a script.
///
/// Each script gets its own _copies_ of the standard library tables
/// so mutations in one script cannot affect another.
pub fn new_script_env(lua: &Lua) -> LuaResult<mlua::Table> {
    let env = lua.create_table()?;
    let globals = lua.globals();

    // Safe scalar builtins — share references (these are functions, immutable).
    let safe_functions: &[&str] = &[
        "assert", "error", "ipairs", "next", "pairs", "pcall", "print", "select", "tonumber",
        "tostring", "type", "unpack", "xpcall",
    ];
    for name in safe_functions {
        if let Ok(val) = globals.raw_get::<mlua::Value>(*name) {
            env.raw_set(*name, val)?;
        }
    }

    // Safe standard libraries — shallow-copy each table so scripts get
    // independent copies and cannot pollute each other.
    let safe_libs: &[&str] = &["string", "table", "math", "utf8", "coroutine"];
    for name in safe_libs {
        if let Ok(mlua::Value::Table(lib)) = globals.raw_get::<mlua::Value>(*name) {
            let copy = shallow_copy_table(lua, &lib)?;
            env.raw_set(*name, copy)?;
        }
    }

    // Self-reference so scripts can declare locals normally.
    env.raw_set("_ENV", env.clone())?;

    Ok(env)
}

/// Create a shallow copy of a Lua table.
fn shallow_copy_table(lua: &Lua, src: &mlua::Table) -> LuaResult<mlua::Table> {
    let dst = lua.create_table()?;
    for pair in src.pairs::<mlua::Value, mlua::Value>() {
        let (k, v) = pair?;
        dst.raw_set(k, v)?;
    }
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandboxed_lua() -> (Lua, Deadline) {
        let lua = Lua::new();
        let deadline = apply(&lua).unwrap();
        deadline.reset(Duration::from_secs(5));
        (lua, deadline)
    }

    #[test]
    fn dangerous_globals_are_removed() {
        let (lua, _dl) = sandboxed_lua();
        for name in DANGEROUS_GLOBALS {
            let val: mlua::Value = lua.globals().get(*name).unwrap();
            assert!(val.is_nil(), "expected {name} to be nil, got {val:?}");
        }
    }

    #[test]
    fn safe_globals_survive() {
        let (lua, _dl) = sandboxed_lua();
        let env = new_script_env(&lua).unwrap();
        for name in &["string", "table", "math", "pairs", "type"] {
            let val: mlua::Value = env.get(*name).unwrap();
            assert!(!val.is_nil(), "expected {name} to be present");
        }
    }

    #[test]
    fn stdlib_tables_are_isolated() {
        let (lua, _dl) = sandboxed_lua();
        let env1 = new_script_env(&lua).unwrap();
        let env2 = new_script_env(&lua).unwrap();

        // Mutate math in env1.
        lua.load("math.custom_field = 42")
            .set_environment(env1)
            .exec()
            .unwrap();

        // env2's math should not see it.
        let result: mlua::Value = lua
            .load("return math.custom_field")
            .set_environment(env2)
            .eval()
            .unwrap();
        assert!(result.is_nil(), "expected nil, got {result:?}");
    }

    #[test]
    fn os_execute_blocked() {
        let (lua, _dl) = sandboxed_lua();
        let env = new_script_env(&lua).unwrap();
        let result = lua
            .load("os.execute('echo pwned')")
            .set_environment(env)
            .exec();
        assert!(result.is_err());
    }

    #[test]
    fn io_open_blocked() {
        let (lua, _dl) = sandboxed_lua();
        let env = new_script_env(&lua).unwrap();
        let result = lua
            .load("io.open('/etc/passwd')")
            .set_environment(env)
            .exec();
        assert!(result.is_err());
    }

    #[test]
    fn memory_limit_enforced() {
        let (lua, _dl) = sandboxed_lua();
        let env = new_script_env(&lua).unwrap();
        let result = lua
            .load("local s = '' for i = 1, 10000000 do s = s .. 'x' end")
            .set_environment(env)
            .exec();
        assert!(result.is_err());
    }

    #[test]
    fn instruction_limit_enforced() {
        let lua = Lua::new();
        let deadline = apply(&lua).unwrap();
        // 50ms deadline — the infinite loop should be killed.
        deadline.reset(Duration::from_millis(50));
        let env = new_script_env(&lua).unwrap();
        let result = lua.load("while true do end").set_environment(env).exec();
        assert!(result.is_err());
    }
}
