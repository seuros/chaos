+++
title = "chaos-mcp(7)"
summary = "MCP client and server usage."
+++

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

### Refreshing provider models

Call `refresh_models` with the required `provider` argument set to a configured
provider ID. It forces discovery using that provider's configured credentials,
updates the catalog cache, and returns an object containing `provider` and `models`.
It does not start a session or switch the current session's provider.
Unknown providers, authoritative custom catalogs, and fetch failures return
errors rather than silently serving stale models.

The tool is also available inside ChaOS sessions and through the session MCP
bridge. Reading `chaos://models` remains cache-only; use `refresh_models` when
a provider's catalog is empty or out of date.

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

### Scheduled shell commands

Pass `schedule` as a JSON object, not a JSON-encoded string. For example,
`{"kind":"interval","seconds":30}` runs every 30 seconds. Daily schedules use
`{"kind":"daily","hour":9,"minute":0}` and weekly schedules add
`"weekday":"mon"` with `"kind":"weekly"`.

The native `cron_create` tool persists a kernel-issued command/policy binding,
not a raw command to run with the next process's privileges. Shell jobs use
`/bin/sh -c` (non-login), the normal sandbox runner, bounded output, and a
60-second execution deadline. Policy/config loading has a separate 10-second
deadline. Shutdown waits for the current bounded shell run before stopping.

Creation requires unattended command authorization under both current session
rules and config-file rules. A command needing approval must have an
operator-managed durable execpolicy allow rule; one-shot approvals, temporary
filesystem/network grants, and allow-rule sandbox bypass are not persisted.
The base filesystem/network policy and shell environment must match the
config-file configuration. Managed proxies and external-sandbox profiles are
not supported for shell cron.

Before every run, including after restart, the kernel reloads config/rules and
checks the saved command, ownership, filesystem/network policy, and launching
process's constraints. Policy differences fail closed: recreate the job under
the new configuration. The environment is freshly filtered using the current
config-file shell environment policy; shell snapshots and credentials are not
stored in the job.
The environment policy must also match the launching process's policy; restart
the scheduler under matching configuration after changing it.

Legacy shell jobs without a policy must be recreated, not merely re-enabled.
Direct MCP catalog calls cannot manufacture this authorization. Use
`cron_toggle` to disable/delete future runs or change config-file rules to
revoke authorization; changing a turn's temporary grant is not a durable job
revocation. Disabling does not interrupt an already-running command.

The scheduler is still process-local: do not run multiple scheduler processes
against the same store when duplicate shell effects are unacceptable. Durable
claims, retries, and per-run records remain separate work. Spool polling is
unchanged.

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
| `chaos://machine` | Read fresh host profile, power, thermals, and relevant filesystem space |
| `chaos://man` | List embedded manual pages and their resource URIs |
| `chaos://man/{page}` | Read an embedded manual page without its frontmatter |

These built-in resources are read-on-demand snapshots. The standalone
`chaos mcp serve` endpoint does not support subscriptions to them; read them
again to refresh their contents.

`chaos://mcp` reports each server's enabled, required, transport, authentication,
and startup state. Failed startup states include the error. Commands, endpoints,
environment variables, headers, and credentials are omitted. Per-session startup
state is available when the resource is read from an active ChaOS session; the
standalone `chaos mcp serve` endpoint reports it as unavailable because it has no
single session to inspect.

`chaos://machine` returns compact JSON containing `scope: "harness_host"`,
`machine`, `storage`, and a `warnings` array. When warnings are active,
`warning_instruction` contains the model-facing checkpoint/operator instruction.
Each read collects new observations on a blocking worker. In-session reads use
the active turn's cwd; standalone MCP reads use the
server's configured cwd. Storage is scoped to that cwd, the configured ChaOS
home/cache/log directories, and the process temporary directory, deduplicated by
filesystem. Other output/checkpoint paths, custom database URL locations,
per-command temp overrides, and remote tool hosts are not inferred.

Unknown power/thermal values and storage probe failures are not evidence of
safety. macOS provides thermal warning/pressure state rather than CPU Celsius
readings. Empty warnings are not safety clearance.

### Machine warnings

The kernel checks the host before each normal model request, including requests
after tool batches and stream retries. Active warnings are request-local developer
instructions, not permanent conversation history: recovered conditions are
re-evaluated rather than accumulating stale alerts. The model is told to save a
minimal checkpoint to verified persistent storage outside temporary directories,
verify the write, pause heavy/interruption-sensitive work, and notify the operator.
It must not assume suspend prevents data loss or automatically delete files.

Global `machine_warnings` settings use the normal settings database and
`chaos config set` / `chaos config unset` interface, not bootstrap `config.toml`.
Restart to apply changes. Project configuration cannot override this host policy.

| Setting | Default | Meaning |
|---|---|---|
| `machine_warnings.enabled` | `true` | Enable warning evaluation and automatic model-request checks |
| `machine_warnings.probe_timeout_ms` | `3000` | Observation wait budget, including queued probes; integer 1–60000 ms |
| `machine_warnings.battery_percent` | `5` | Warn at or below this charge percentage; integer 0–100 |
| `machine_warnings.disk_free_percent` | `5` | Warn at or below this caller-available percentage; integer 0–100 |
| `machine_warnings.thermal` | `true` | Warn on OS thermal warning/critical state or a CPU channel reaching its own critical limit |
| `machine_warnings.cpu_temperature_celsius` | unset | Additional physical CPU temperature threshold; finite −100–250°C, inclusive |

Battery warnings require a present/not-known-absent, discharging battery of the
current system/UPS power source, without external power. Dead, removed, idle, or
maintenance-discharging batteries on AC do not trigger them. Multiple batteries
remain individual observations, never an invented combined percentage.
Disk warnings cover only the relevant paths listed above, once per filesystem.
Unknown percentages and failed probes do not become zero free space.

Thermal warnings do not depend on laptop classification. Custom Celsius thresholds
apply only to physical readings, never AMD Tctl or unknown temperature scales;
driver critical limits apply only to their own channel. An unknown thermal state
alone does not trigger an overheating warning. Disabling `thermal` disables both
OS/sensor and custom-temperature warnings.

Probe waits default to three seconds with at most one blocking worker active.
The budget can be increased for slow or restricted host APIs.
A failed/timed-out model-request check asks for operator verification before risky
work rather than inventing measurements. Model checks are not a background monitor and
cannot interrupt a running tool, automatically save work, or block execution.
Standalone MCP clients receive warnings when reading the resource, not pushed
notifications.

The terminal top bar polls the same bounded collector and policy for display
(see `chaos-appearance(7)`). It follows the active UI workspace, never all disks,
and does not push model notifications or replace fresh pre-request checks.

### Resource tool rules

- Read internal resources with `read_mcp_resource({"uri":"chaos://sessions"})`
  (replace the URI as needed). Omit `server` for internal reads, subscriptions,
  and task cancellation. External MCP resources and tasks require their configured
  `server`; supplied names never fall back to internal routing.
- Listing resources or templates without `server` aggregates internal and external
  entries. Internal entries omit `server`; external entries retain it.
- Internal tasks use `tasks://`, `tasks://get/<id>`, and `tasks://result/<id>`
  without `server`. Execution and agent outputs provide `task_id`, not `task_server`.
  `call_mcp_tool_async` remains external-only and requires `server`.
- **Breaking change:** `server: "chaos_local"` is rejected; omit the field instead.
  Internal tool labels have no server prefix, and invocation events omit `server`.
  Actual external MCP tool invocations retain their server metadata and prefix.
- If a subscription call returns `fallback: "read_once"`, consume `snapshot`
  as a successful read. Do not retry the subscription or report it as a failure.
- `subscribed: false` means no updates will arrive. Do not claim to be watching
  the resource. Call `read_mcp_resource` again only when fresh data is needed.
- `fallback: "no_op"` acknowledges unsubscribe; no further action is needed.
- For configured MCP servers, subscribe only when subscription support is
  advertised. Server errors are never redirected to internal resources.
- If a read fails, use the error to correct the server, URI, or access issue;
  resource access checks remain enforced for internal reads and snapshots.

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
