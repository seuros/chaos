# chaos-clamp

First-party model CLI subprocess transports.

For Google policy restrictions, account-risk context, rejected approaches, and
the full Antigravity security design, see
[`docs/antigravity-oauth-risk-and-design.md`](../../../docs/antigravity-oauth-risk-and-design.md).

- **Claude Code:** bidirectional stream-JSON control transport. Chaos owns
  tools and permissions through its MCP bridge.
- **Google Antigravity (`agy`):** one subprocess per model turn with explicit
  provider conversation resume and normalized JSONL usage. `agy` runs
  sandboxed with native command, filesystem, unsandboxed, and URL operations
  denied. Its sole action surface is the session-scoped Chaos MCP bridge.
  Metered Gemini API-key environment variables are removed, and the ephemeral
  bridge socket/token are inherited by the MCP child without being written to
  persistent configuration.

Every Antigravity turn also starts a loopback `CONNECT` proxy and runs `agy`
under the platform sandbox helper. The helper permits exactly one TCP
destination — the port the proxy bound — and the proxy answers `403` for any
host outside its allowlist. Landlock network rules are port-scoped rather than
host-scoped, which is why the destination policy lives in the proxy while the
kernel supplies the enforcement. A CLI that ignores `HTTPS_PROXY` reaches
nothing. When no sandbox helper is available the turn still runs proxied, and
the missing confinement is logged.

The proxy terminates TLS with a per-session certificate authority written
owner-only next to the conversation state and exported as `SSL_CERT_FILE`, so
request and response bodies land in the same wiretap sink as the Claude Code
transport. A CLI that pins certificates would need the relay mode instead,
which keeps the allowlist and loses body visibility.

As with Claude Code, authentication remains an external responsibility of the
official provider CLI:

```bash
export CHAOS_AGY_HOME=/private/antigravity-state
env -u GEMINI_API_KEY -u GOOGLE_API_KEY \
  HOME="$CHAOS_AGY_HOME" \
  XDG_CONFIG_HOME="$CHAOS_AGY_HOME/.config" \
  "${CHAOS_AGY_PATH:-agy}" models
```

Every knob is settable in `config.toml`, and each key has an environment
override for one-off runs:

```toml
[antigravity]
cli_path = "/opt/antigravity/bin/agy"   # CHAOS_AGY_PATH
home = "/private/antigravity-state"     # CHAOS_AGY_HOME
cwd = "/srv/workspaces/agy"             # CHAOS_AGY_CWD
model = "gemini-3.1-pro-high"           # CHAOS_AGY_MODEL
conversation_dir = "/private/agy-state" # CHAOS_AGY_CONVERSATION_DIR
print_timeout_seconds = 900             # CHAOS_AGY_PRINT_TIMEOUT_SECONDS
```

`model` bypasses the derived slug entirely, which is the escape hatch when
Google renames a model or ships a tier this build does not know about.

`CHAOS_AGY_HOME` must be a dedicated persistent private directory outside the
source checkout. Chaos preserves OAuth and unrelated top-level settings but
owns the effective MCP server list and permission policy inside this home.
`CHAOS_AGY_PATH` may pin the official CLI binary. Chaos does not wrap provider
login, status, or logout in a separate clamp command namespace. Service
integrations must preserve the Chaos process ID and this home directory, and
must pass the original `-m` model selection again, to resume the same provider
conversation across operating-system processes.
