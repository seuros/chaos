# MCP — Model Context Protocol

Chaos uses MCP to connect to external tools and services. MCP servers are
the drivers that give Chaos its capabilities — file access, shell commands,
APIs, databases, anything.

Chaos is also an MCP server itself, so other tools can drive it.

---

## Connecting to MCP servers

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

### Config file

Servers are stored in `~/.codex/config.toml`:

```toml
[mcp_servers.filesystem]
command = "bunx"
args = ["@modelcontextprotocol/server-filesystem", "/home/user"]

[mcp_servers.remote-api]
type = "streamable_http"
url = "https://api.example.com/mcp"
```

#### Stdio server options

```toml
[mcp_servers.my-server]
command = "my-mcp-server"
args = ["--port", "3000"]
env = { MY_TOKEN = "secret" }
cwd = "/opt/my-server"
enabled_tools = ["read", "write"]     # Only expose these tools
disabled_tools = ["delete"]            # Or block specific tools
startup_timeout_seconds = 10
tool_timeout_seconds = 120
```

#### HTTP server options

```toml
[mcp_servers.remote]
type = "streamable_http"
url = "https://api.example.com/mcp"
bearer_token_env = "REMOTE_API_KEY"   # Auth from env var
```

---

## Using Chaos as an MCP server

Other tools can control Chaos through MCP:

```bash
chaos mcp serve
```

This starts Chaos as a stdio MCP server exposing:

### Tools

| Tool | Description |
|------|-------------|
| `chaos` | Start or resume a Chaos session |

### Resources

| URI | Description |
|-----|-------------|
| `chaos://sessions` | List all sessions |
| `chaos://sessions/{id}` | Read session details |
| `chaos://crons` | List scheduled jobs |

### Integrating with other harnesses

Add Chaos to another MCP client's config:

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

---

## Transports

| Transport | Status | Use case |
|-----------|--------|----------|
| Stdio | Supported | Local processes, CLI tools |
| Streamable HTTP (SSE) | Supported | Remote servers, APIs |

---

## Protocol support

### What works

- **Tools** — List, call, pagination, change notifications. Tool calls
  respect sandbox policies.
- **Resources** — List, read, subscribe to updates, templates.
- **Prompts** — List and get with pagination.
- **Sampling** — Server can request the LLM to generate text.
- **Elicitation** — Server can ask the user for approval (form or URL).
- **Roots** — Server can discover project roots.
- **Logging** — Set log level, receive log messages.
- **Tasks** — List, get, cancel async operations.
- **Notifications** — Tools/resources/prompts change notifications,
  progress updates.

### What's missing

- **OAuth flows** — Types exist, not yet wired.

---

## How tools are named

When the LLM sees tools from MCP servers, they're prefixed:

```
mcp__<server>__<tool>
```

For example, a `read` tool from a server named `filesystem` becomes
`mcp__filesystem__read`. Names longer than 64 characters are hashed.

---

## Timeouts

| Setting | Default | Description |
|---------|---------|-------------|
| `startup_timeout_seconds` | 10 | Time to wait for server to initialize |
| `tool_timeout_seconds` | 120 | Time to wait for a tool call to complete |

Servers that fail to start within the timeout are skipped. Tool calls
that exceed the timeout return an error.

---

## Troubleshooting

**Server fails to start** — Check the command exists and runs manually.
Look at `~/.chaos/log/` for connection errors.

**Tool not appearing** — Run `chaos mcp list` to verify the server is
configured. Check `enabled_tools`/`disabled_tools` filters.

**Timeouts** — Increase `tool_timeout_seconds` for slow operations:

```toml
[mcp_servers.slow-server]
command = "slow-tool"
tool_timeout_seconds = 300
```

**Connection drops** — HTTP servers reconnect automatically. Stdio servers
are restarted if they crash.
