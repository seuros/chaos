# chaos-mcp(7)

## NAME

chaos-mcp - connect FreeChaOS to MCP servers and expose FreeChaOS over MCP

## DESCRIPTION

FreeChaOS uses MCP to connect to external tools and services. MCP servers are
the drivers that give FreeChaOS its capabilities - file access, shell commands,
APIs, databases, anything.

FreeChaOS is also an MCP server itself, so other tools can drive it. This page
covers both sides: using external MCP servers from FreeChaOS, and exposing
FreeChaOS over MCP.

## CLIENT USAGE

### Adding a server

```bash
# Stdio transport (local process)
chaos mcp add my-server -- bunx my-mcp-server

# Streamable HTTP transport (remote)
chaos mcp add my-api --url https://api.example.com/mcp
```

### Managing servers

```bash
chaos mcp list              # List all configured servers
chaos mcp get my-server     # Show config for a server
chaos mcp remove my-server  # Remove a server
```

### Storage

Global MCP servers are stored in the runtime MCP registry, not in
`~/.chaos/config.toml`. Use `chaos mcp add`, `chaos mcp remove`, and
`chaos mcp get` to manage them.

Project-local MCP servers still live in `.mcp.json`.

### Project-local `.mcp.json`

Place a `.mcp.json` in your project root for cross-harness configuration shared
with FreeChaOS, Claude Code, and other MCP-aware tools:

```json
{
  "mcpServers": {
    "my-server": {
      "command": "bun",
      "args": ["server.js"],
      "env": {
        "API_KEY": "..."
      }
    }
  }
}
```

### Agent-facing MCP management tools

Agents can manage project-local servers with:

| Tool | Purpose |
|------|---------|
| `mcp_add_server` | Add a stdio or HTTP server and reload the active session |
| `mcp_server` | Enable, disable, reset, or remove a server and reload the active session |

Use `command` for stdio or `url` for HTTP.

### Trusted stdio identity

Each managed stdio server receives a host-controlled `CHAOS_MCP_CLIENT_ID`.
The identity is scoped to the persisted ChaOS conversation and server name, so
it survives server resets and harness restarts that resume the same
conversation. Different conversations and different servers receive distinct
identities. Server configuration cannot override the value.

The identity preserves server-side ownership across reconnects; it does not
extend or revive an expired server-side lease. A server should resume a live
lease for this identity or create a new fenced generation after expiration.

### Persistent tool approvals

Personal MCP trust decisions belong in `~/.chaos/config.toml`, separate from
shared project server definitions:

```toml
[mcp_tool_approvals.browser]
approval_mode = "approve"

[mcp_tool_approvals.browser.tools]
evaluate = "prompt"
```

The modes are:

- `auto` — use the tool's MCP annotations to decide whether to ask.
- `prompt` — always ask before running the tool.
- `approve` — skip the ordinary approval prompt. Safety monitoring can still
  interrupt or ask for confirmation when it detects unusual risk.

Per-tool settings take precedence over the server default. Selecting **Allow
and don't ask me again** in an approval prompt persists `approve` for that
specific server and tool in `~/.chaos/config.toml`. Connector/app approvals use
their existing `[apps]` entries in the same file.

#### Stdio server options

```bash
chaos mcp add my-server -- my-mcp-server --port 3000
```

#### HTTP server options

```bash
chaos mcp add remote --url https://api.example.com/mcp \
  --bearer-token-env-var REMOTE_API_KEY

# Or store the token directly in the runtime MCP registry
chaos mcp add remote --url https://api.example.com/mcp \
  --bearer-token "$REMOTE_API_KEY"
```

## SERVER USAGE

Start the server:

```bash
chaos mcp serve
```

Pipe it into another client:

```bash
chaos mcp serve | your_mcp_client
```

For inspection:

```bash
bunx @mcpjam/inspector@latest chaos mcp serve
```

The server runs over standard MCP stdio transport using JSON-RPC 2.0.

## INTEGRATION

Add FreeChaOS to another MCP client's config:

```json
{
  "mcpServers": {
    "chaos": {
      "command": "chaos",
      "args": ["mcp", "serve"]
    }
  }
}
```

## EXPOSED INTERFACE

### Tool

| Tool | Description |
|------|-------------|
| `chaos` | Start, resume, or continue a FreeChaOS process through the unified MCP tool |

The older split between `chaos` and `chaos-reply` has been replaced by this
single unified `chaos` tool.

### Resources

| URI | Description |
|-----|-------------|
| `chaos://sessions` | List all sessions |
| `chaos://sessions/{id}` | Read session details |
| `chaos://crons` | List scheduled jobs |
| `chaos://spool` | List persisted spool jobs |
| `chaos://models` | List available model presets |
| `chaos://modes` | List the caller-visible collaboration mode catalog |
| `chaos://mcp` | List configured MCP servers with auth and startup status |
| `chaos://man` | List agent-facing manual pages and their resource URIs |
| `chaos://man/{page}` | Read an agent-facing manual page |

`chaos://mcp` reports each server's enabled, required, transport, authentication,
and startup state. Failed startup states include the error. Commands, endpoints,
environment variables, headers, and credentials are omitted. Per-session startup
state is available when the resource is read from an active ChaOS session; the
standalone `chaos mcp serve` endpoint reports it as unavailable because it has no
single session to inspect.

### Events and approvals

While a conversation runs, the server emits `chaos/event` notifications for
live agent events.

When FreeChaOS needs approval to apply changes or run commands, it sends an MCP
`elicitation/create` request. The client replies with:

```json
{
  "action": "accept" | "decline" | "cancel",
  "content": {}
}
```

Tool results are returned as normal MCP `CallToolResult` payloads with
structured content.

## TRANSPORTS

| Transport | Status | Use case |
|-----------|--------|----------|
| Stdio | Supported | Local processes, CLI tools |
| Streamable HTTP (SSE) | Supported | Remote servers, APIs |

## PROTOCOL SUPPORT

### What works

- **Tools** — listing, calling, pagination, and change notifications
- **Resources** — listing, reading, templates, and update flows
- **Prompts** — list/get support with pagination where applicable
- **Sampling** — server-side requests for model output
- **Elicitation** — approval and user-input requests
- **Roots** — project root discovery
- **Logging** — log level and message transport
- **Tasks** — async task discovery and cancellation
- **Notifications** — progress and live event delivery

### Stability

The MCP surface is real and in use, but specific method names, fields, and
event shapes may still evolve with the implementation.

## TOOL NAMING

When the LLM sees tools from MCP servers, they are prefixed:

```
mcp__<server>__<tool>
```

For example, a `read` tool from a server named `filesystem` becomes
`mcp__filesystem__read`. Names longer than 64 characters are hashed.

## TIMEOUTS

| Setting | Default | Description |
|---------|---------|-------------|
| `startup_timeout_seconds` | 10 | Time to wait for server initialization |
| `tool_timeout_seconds` | 120 | Time to wait for a tool call to complete |

Servers that fail to start within the timeout are skipped. Tool calls that
exceed the timeout return an error.

## TROUBLESHOOTING

**Server fails to start** — Check that the command exists and runs manually.
Read `chaos://mcp` for the server's startup state and error, then look at
`~/.chaos/log/` for additional connection details.

**Tool not appearing** — Run `chaos mcp list` to verify the server is
configured. Check any `enabled_tools` / `disabled_tools` filters.

**Timeouts** — Review the server entry and increase startup or tool timeout
settings if the remote side is slow.

**Connection drops** — HTTP servers reconnect automatically. Stdio servers are
restarted if they crash.

## FILES

- `.mcp.json` - project-local MCP server definitions
- `~/.chaos/config.toml` - general user config
- runtime MCP registry - global MCP server storage used by `chaos mcp ...`

## SEE ALSO

- [chaos-install.7](./chaos-install.7.md)
- [chaos-modes.7](./chaos-modes.7.md)
- [chaos-providers.7](./chaos-providers.7.md)
- [chaos-httpd.8](./chaos-httpd.8.md)
