# Hallucinate — Scripting Engine

Hallucinate lets you teach FreeChaOS new tricks without recompiling. Drop a Lua
script into the right folder and the LLM can call it as a tool.

---

## Quick start

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

---

## Script locations

Scripts are loaded automatically on session startup from two places:

| Layer | Path | Priority |
|-------|------|----------|
| User | `~/.config/chaos/scripts/` | Lower |
| Project | `.chaos/scripts/` (relative to working directory) | Higher |

Project scripts load after user scripts and can override tools with the
same name. Scripts load in alphabetical order within each layer.

Only `.lua` files are loaded. WASM support is planned.

---

## API reference

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

---

## Available Lua globals

Scripts run in a sandbox. The following standard libraries are available:

- `string`, `table`, `math`, `utf8`, `coroutine`
- `assert`, `error`, `ipairs`, `next`, `pairs`, `pcall`, `print`,
  `select`, `tonumber`, `tostring`, `type`, `unpack`, `xpcall`

The following are **blocked**:

- `os`, `io`, `debug`, `package`
- `require`, `load`, `loadfile`, `dofile`, `collectgarbage`

Each script gets its own isolated environment — scripts cannot interfere
with each other.

---

## Limits

| Limit | Value |
|-------|-------|
| Memory per script | 8 MiB |
| Execution time per tool call | 10 seconds |
| Batch load timeout | 30 seconds |

If a script exceeds these limits, the call fails and FreeChaOS continues
without it.

---

## Examples

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

---

## Troubleshooting

**Tool doesn't appear** — Check the file is in `~/.config/chaos/scripts/`
or `.chaos/scripts/` and has a `.lua` extension. Check logs for load errors.

**Script fails silently** — Look at `~/.chaos/log/` for error details.
Common causes: syntax errors, exceeding memory or time limits.

**Tool returns wrong result** — The handler must return a string. If you
return a table or nil, the result will be empty.
