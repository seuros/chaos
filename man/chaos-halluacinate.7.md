# chaos-halluacinate(7)

## NAME

chaos-halluacinate - extend FreeChaOS with Lua tools, hooks, and statusline scripts

## DESCRIPTION

Halluacinate lets you teach FreeChaOS new tricks without recompiling. Drop a Lua
script into the right folder and the LLM can call it as a tool.

## QUICK START

Create a script at `~/.config/chaos/scripts/hello.lua`:

```lua
chaos.tool({
    name = "hello",
    description = "Greet someone by name",
    input_schema = {
        type = "object",
        properties = {
            name = { type = "string", description = "Who to greet" }
        },
        required = { "name" }
    },
    handler = function(args)
        return "Hello, " .. args.name .. "!"
    end
})
```

Start a session. The LLM now has a `hello` tool it can call.

Halluacinate also owns the TUI status line. ChaOS ships a built-in Lua renderer,
and you can override it by registering your own `chaos.statusline(...)` script.
The built-in script loads first, then user scripts, then project scripts, so the
last registered statusline wins.

## FILES

Scripts are loaded automatically on session startup from two places:

| Layer | Path | Priority |
|-------|------|----------|
| User | `~/.config/chaos/scripts/` | Lower |
| Project | `.chaos/scripts/` (relative to working directory) | Higher |

Project scripts load after user scripts and can override tools with the
same name. Scripts load in alphabetical order within each layer.

Only `.lua` files are loaded. WASM support is planned.

## API

### Session context

Read-only information about the current session:

```lua
chaos.session_id   -- unique conversation ID
chaos.cwd          -- working directory
chaos.provider     -- provider name (e.g. "anthropic", "openai")
```

### Registering tools

```lua
chaos.tool({
    name = "tool_name",
    description = "What it does — the LLM reads this",
    input_schema = {
        type = "object",
        properties = {
            param = { type = "string", description = "..." }
        },
        required = { "param" }
    },
    handler = function(args)
        -- args is a Lua table with parsed parameters
        -- return a string with the result
        return "done"
    end
})
```

The `input_schema` follows JSON Schema. The LLM uses it to understand what
parameters to pass.

### Hooks

React to events during the session:

```lua
chaos.on("event_name", function(payload)
    -- payload is a Lua table
    chaos.log.info("event fired")
end)
```

### Logging

```lua
chaos.log.info("message")
chaos.log.warn("message")
chaos.log.debug("message")
```

Logs go to the FreeChaOS log file (`~/.chaos/log/`).

### Status line

Override the default TUI status line:

```lua
chaos.statusline(function(ctx)
    return {
        { text = ctx.model, bold = true },
        { text = " · " },
        { text = tostring(ctx.context.remaining_pct) .. "% left" },
        { text = " · " },
        { text = ctx.cwd_display or ctx.cwd },
    }
end)
```

Place a statusline script in either of the normal script layers:

- Per-user: `~/.config/chaos/scripts/statusline.lua`
- Per-project: `.chaos/scripts/statusline.lua`

The project copy overrides the user copy because project scripts load later.

The built-in renderer is a Doom-style HUD. By default it shows:

- `HUD` so it is obviously not the old Rust footer
- `HP`, where remaining context is treated as health and color-coded
- `CRIT` when context health gets dangerously low
- `WPN`, showing the active model and reasoning effort
- `MAP`, using the current branch, project root, or directory
- `ARM`, showing sandbox and approval posture
- `CTX`, showing effective context load above the fixed prompt baseline
- `AMMO`, showing last-response output with optional context growth
- `DIR`, when it adds extra path context beyond `MAP`

If multiple scripts call `chaos.statusline(...)`, the last loaded script wins.

The `ctx` table currently includes:

```lua
ctx.model
ctx.reasoning_effort
ctx.provider
ctx.branch
ctx.cwd
ctx.cwd_display
ctx.project_root
ctx.approval
ctx.sandbox
ctx.version
ctx.session_id
ctx.context.remaining_pct
ctx.context.used_pct
ctx.context.window_size
ctx.tokens.available
ctx.tokens.used
ctx.tokens.input
ctx.tokens.output
ctx.tokens.blended
ctx.tokens.context_raw
ctx.tokens.context_effective
ctx.tokens.last_raw
ctx.tokens.last_effective
ctx.tokens.last_input
ctx.tokens.last_output
ctx.tokens.last_blended
ctx.five_hour
ctx.weekly
```

`ctx.tokens.used/input/output` are the legacy cumulative counters. For statusline
UX, prefer the explicit fields like `ctx.tokens.context_effective` and
`ctx.tokens.last_output`.

## SANDBOX

Scripts run in a sandbox. The following standard libraries are available:

- `string`, `table`, `math`, `utf8`, `coroutine`
- `assert`, `error`, `ipairs`, `next`, `pairs`, `pcall`, `print`,
  `select`, `tonumber`, `tostring`, `type`, `unpack`, `xpcall`

The following are **blocked**:

- `os`, `io`, `debug`, `package`
- `require`, `load`, `loadfile`, `dofile`, `collectgarbage`

Each script gets its own isolated environment — scripts cannot interfere
with each other.

## LIMITS

| Limit | Value |
|-------|-------|
| Memory per script | 8 MiB |
| Execution time per tool call | 10 seconds |
| Batch load timeout | 30 seconds |

If a script exceeds these limits, the call fails and FreeChaOS continues
without it.

## EXAMPLES

### Timestamp tool

```lua
chaos.tool({
    name = "timestamp",
    description = "Return the current UTC timestamp",
    input_schema = { type = "object", properties = {} },
    handler = function()
        -- os.time is blocked, but you can use other approaches
        return tostring(chaos.session_id) .. " — time is an illusion"
    end
})
```

### String utility

```lua
chaos.tool({
    name = "reverse_string",
    description = "Reverse a string",
    input_schema = {
        type = "object",
        properties = {
            text = { type = "string", description = "Text to reverse" }
        },
        required = { "text" }
    },
    handler = function(args)
        return string.reverse(args.text)
    end
})
```

### Logging hook

```lua
chaos.on("tool_call", function(payload)
    chaos.log.info("Tool called: " .. tostring(payload.name))
end)
```

## TROUBLESHOOTING

**Tool doesn't appear** — Check the file is in `~/.config/chaos/scripts/`
or `.chaos/scripts/` and has a `.lua` extension. Check logs for load errors.

**Script fails silently** — Look at `~/.chaos/log/` for error details.
Common causes: syntax errors, exceeding memory or time limits.

**Tool returns wrong result** — The handler must return a string. If you
return a table or nil, the result will be empty.

## SEE ALSO

- [chaos-install.7](./chaos-install.7.md)
- [chaos-mcp.7](./chaos-mcp.7.md)
