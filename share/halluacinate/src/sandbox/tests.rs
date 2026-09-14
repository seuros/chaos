use super::*;

fn sandboxed_lua() -> (Lua, Deadline) {
    let lua = Lua::new();
    let deadline = apply(&lua).unwrap();
    deadline.reset(Duration::from_secs(5));
    (lua, deadline)
}

#[test]
fn sandbox_suite() {
    dangerous_globals_are_removed();
    safe_globals_survive();
    stdlib_tables_are_isolated();
    os_execute_blocked();
    io_open_blocked();
    memory_limit_enforced();
    instruction_limit_enforced();
}

fn dangerous_globals_are_removed() {
    let (lua, _dl) = sandboxed_lua();
    for name in DANGEROUS_GLOBALS {
        let val: mlua::Value = lua.globals().get(*name).unwrap();
        assert!(val.is_nil(), "expected {name} to be nil, got {val:?}");
    }
}

fn safe_globals_survive() {
    let (lua, _dl) = sandboxed_lua();
    let env = new_script_env(&lua).unwrap();
    for name in &["string", "table", "math", "pairs", "type"] {
        let val: mlua::Value = env.get(*name).unwrap();
        assert!(!val.is_nil(), "expected {name} to be present");
    }
}

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

fn os_execute_blocked() {
    let (lua, _dl) = sandboxed_lua();
    let env = new_script_env(&lua).unwrap();
    let result = lua
        .load("os.execute('echo pwned')")
        .set_environment(env)
        .exec();
    assert!(result.is_err());
}

fn io_open_blocked() {
    let (lua, _dl) = sandboxed_lua();
    let env = new_script_env(&lua).unwrap();
    let result = lua
        .load("io.open('/etc/passwd')")
        .set_environment(env)
        .exec();
    assert!(result.is_err());
}

fn memory_limit_enforced() {
    let (lua, _dl) = sandboxed_lua();
    let env = new_script_env(&lua).unwrap();
    let result = lua
        .load("local s = '' for i = 1, 10000000 do s = s .. 'x' end")
        .set_environment(env)
        .exec();
    assert!(result.is_err());
}

fn instruction_limit_enforced() {
    let lua = Lua::new();
    let deadline = apply(&lua).unwrap();
    // 50ms deadline — the infinite loop should be killed.
    deadline.reset(Duration::from_millis(50));
    let env = new_script_env(&lua).unwrap();
    let result = lua.load("while true do end").set_environment(env).exec();
    assert!(result.is_err());
}
