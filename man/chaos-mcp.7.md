+++
title = "chaos-mcp(7)"
summary = "MCP client and server usage."
+++

# chaos-mcp(7)

## NAME

chaos-mcp - connect FreeChaOS to MCP servers and expose FreeChaOS over MCP

## DESCRIPTION

Connect external tools through MCP servers, or expose FreeChaOS to another MCP
client with `chaos mcp serve`.

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

Manage global servers with `chaos mcp ...`; project-local definitions use `.mcp.json`.

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

### In-session management

Use the native MCP management tools rather than editing `.mcp.json`.
Read `chaos://mcp` before and after changes to check startup and authentication.
Server reconnects preserve conversation identity, not expired server leases.

### Persistent tool approvals

Personal MCP trust decisions are user settings in the database, separate from
shared project server definitions:

```sh
chaos config set mcp_tool_approvals.browser.approval_mode '"auto"'
chaos config set mcp_tool_approvals.browser.tools.evaluate '"prompt"'
```

The modes are:

- `auto` — use the tool's MCP annotations to decide whether to ask.
- `prompt` — always ask before running the tool.
- `approve` — skip the ordinary approval prompt. Safety monitoring can still
  interrupt or ask for confirmation when it detects unusual risk.

Per-tool settings take precedence over the server default. `approve` cannot be
written with `chaos config set`; selecting **Allow and don't ask me again** in
an approval prompt records the grant for that specific server and tool in the
database (`chaos approvals list`). Connector/app approvals use the `apps`
settings the same way.

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

Serving requires a build with `mcpd` enabled (the default).
See [build options](./chaos-install.7.md#build-without-the-tui-acp-or-mcp-daemon).

Start the server:

```bash
chaos mcp serve
```

For inspection:

```bash
bunx @mcpjam/inspector@latest chaos mcp serve
```

### Refreshing provider models

Use `refresh_models` when a provider's catalog is empty or stale.
Reading `chaos://models` does not refresh it; refreshing does not switch providers.

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

### Scheduled shell commands

Shell jobs run with `/bin/sh -c` (non-login), sandboxing, and a 60-second deadline.

Creation requires unattended command authorization under both current session
rules and config-file rules. A command needing approval must have an
operator-managed durable execpolicy allow rule; one-shot approvals, temporary
filesystem/network grants, and allow-rule sandbox bypass are not persisted.
The base filesystem/network policy and shell environment must match the
config-file configuration. Managed proxies and external-sandbox profiles are
not supported for shell cron.

After policy changes, recreate the job and restart the scheduler under matching
configuration. Jobs do not save shell snapshots or credentials.

Legacy shell jobs without a policy must be recreated, not merely re-enabled.
Use `cron_toggle` to disable/delete future runs or change config-file rules to
revoke authorization; changing a turn's temporary grant is not a durable job
revocation. Disabling does not interrupt an already-running command.

Do not run multiple schedulers against the same store when duplicate shell
effects are unacceptable.

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
| `chaos://hooks` | List database lifecycle hooks visible to the caller, revisions, and activation status |
| `chaos://hooks/{id}` | Read one visible hook definition |
| `chaos://machine` | Read host, power, thermal, storage, and indexing status |
| `chaos://man` | List embedded manual pages and their resource URIs |
| `chaos://man/{page}` | Read an embedded manual page without its frontmatter |

For hook management and approval requirements, see
[chaos-hooks(7)](./chaos-hooks.7.md).

These built-in resources are read-on-demand snapshots. The standalone
`chaos mcp serve` endpoint does not support subscriptions to them; read them
again to refresh their contents.

Read `chaos://mcp` inside a live session for startup errors; standalone serving
has no per-session startup state. Credentials are omitted.

`chaos://machine` observes the harness host. Storage checks cover the working
directory, ChaOS home/cache/log paths, and temporary filesystem.
macOS reports thermal pressure.

On macOS, `indexing` reports workspace-volume Spotlight state and visible host
indexers with averaged CPU usage. On Linux, it reports recognized visible
Tracker/LocalSearch, Baloo, and `updatedb` executables with process-lifetime CPU
averages. FreeBSD reports recognized desktop indexers and native `locate`
binary helpers with decaying CPU averages, confirming accounting names through
native executable-path queries. Shell-script parents and generic `find`/`sort`
processes are not attributed to indexing. Spotlight state remains `unsupported`
and volume is null on Linux/FreeBSD. An empty process list does not establish that
indexing is disabled. Probes are read-only and on demand, with explicit
availability status; hidden processes and unrecognized indexers may be missed.

### Machine warnings

On a host warning, checkpoint to verified persistent storage outside temporary
directories, pause heavy/interruption-sensitive work, and notify the operator.
Checkpoint before suspending and preserve existing files.

Manage global `machine_warnings` with `chaos config set` / `chaos config unset`.
Restart to apply changes. Project configuration cannot override this host policy.

| Setting | Default | Meaning |
|---|---|---|
| `machine_warnings.enabled` | `true` | Enable warning evaluation and automatic model-request checks |
| `machine_warnings.probe_timeout_ms` | `3000` | Observation wait budget, including queued probes; integer 1–60000 ms |
| `machine_warnings.battery_percent` | `5` | Warn at or below this charge percentage; integer 0–100 |
| `machine_warnings.disk_free_percent` | `5` | Warn at or below this caller-available percentage and the byte cap; integer 0–100 |
| `machine_warnings.disk_free_bytes_cap` | `21474836480` | Cap the warning reserve at 20 GiB; positive integer bytes |
| `machine_warnings.thermal` | `true` | Warn on OS thermal warning/critical state or a CPU channel reaching its own critical limit |
| `machine_warnings.cpu_temperature_celsius` | unset | Additional physical CPU temperature threshold; finite −100–250°C, inclusive |
| `machine_warnings.recovery_stable_seconds` | `300` | Continuous observed recovery before an opted-in wake; integer 30–86400 seconds |
| `machine_warnings.recovery_temperature_margin_celsius` | `5.0` | Headroom below a triggering sensor's threshold; finite 0.1–100 degrees on that channel's scale |
| `machine_warnings.recovery_disk_margin_percent` | `2` | Additional free percentage points for recovery, subject to the byte cap; integer 0–100 |
| `machine_warnings.recovery_disk_margin_bytes` | `1073741824` | Additional free bytes above the capped warning reserve; also required for recovery |

Disk warnings require both limits: `available <= min(total * percent / 100, cap)`.
A 4 TB disk with 200 GB available does not warn; the default reserve caps at
20 GiB. Small disks retain percentage-based reserves. Existing configurations
inherit the cap unless overridden.

Disk recovery requires the greater of the capped percentage-with-margin reserve
and the capped warning reserve plus the byte margin. Percentage-derived recovery
reserves round up to whole bytes. Defaults require 21 GiB on large disks, followed
by the stable observation window—not 7% of their capacity. Impossible recovery
targets remain blocked. This policy does not predict depletion or guarantee a
large write will fit.

### Opt-in recovery wake

After checkpointing and stopping heavy background work, call
`wait_for_machine_recovery` **alone** to request one recovery wake. It does not
stop processes, suspend the host, or restart commands.

Recovery normally requires five continuously observed stable minutes: external
power for battery warnings, normal thermal state, and the configured temperature
and disk margins. Missing measurements and gaps over 60 seconds reset the window;
suspend time does not count. On waking, reassess rather than blindly retrying.

Input, interruption, workspace/policy changes, closure, and restart cancel waits.
Read `chaos://machine` for recovery progress. Without an opted-in wait, recovery
does not start a new model turn.

Warnings cannot interrupt tools or save work automatically. Failed probes require
operator verification before risky work. Standalone MCP clients must reread the
resource; warnings are not pushed. See [chaos-appearance(7)](./chaos-appearance.7.md)
for UI indicators.

### Resource tool rules

- Use resource listing for discovery; prefer resources over tool calls or web
  searches when they already contain the needed data.
- For templates, pass raw values in `parameters`; do not pre-encode them.
  For concrete URIs, omit `parameters`.
- Follow async work through its task handle. Internal tasks use `tasks://get/<id>`
  and `tasks://result/<id>`; external tasks require their originating server.
  Cancel explicitly; stopping polling does not stop work.
- If a subscription call returns `fallback: "read_once"`, consume `snapshot`
  as a successful read. Do not retry the subscription or report it as a failure.
- `subscribed: false` means no updates will arrive. Do not claim to be watching
  the resource. Call `read_mcp_resource` again only when fresh data is needed.
- `fallback: "no_op"` acknowledges unsubscribe; no further action is needed.
- Subscribe only when the server advertises support. On failure, correct the
  server, URI, or access issue rather than retrying blindly.

### Events and approvals

While a conversation runs, the server emits `chaos/event` notifications for
live agent events.

Clients must handle MCP `elicitation/create` for approval requests, replying with
`accept`, `decline`, or `cancel` and the requested form content.

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
- `~/.chaos/config.toml` - bootstrap: `storage_url` and `egress_url` only
- runtime MCP registry - global MCP server storage used by `chaos mcp ...`

## SEE ALSO

- [chaos-install.7](./chaos-install.7.md)
- [chaos-hooks.7](./chaos-hooks.7.md)
- [chaos-modes.7](./chaos-modes.7.md)
- [chaos-providers.7](./chaos-providers.7.md)
- [chaos-httpd.8](./chaos-httpd.8.md)
