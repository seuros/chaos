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

As with Claude Code, authentication remains an external responsibility of the
official provider CLI:

```bash
export CHAOS_AGY_HOME=/private/antigravity-state
env -u GEMINI_API_KEY -u GOOGLE_API_KEY \
  HOME="$CHAOS_AGY_HOME" \
  XDG_CONFIG_HOME="$CHAOS_AGY_HOME/.config" \
  "${CHAOS_AGY_PATH:-agy}" models
```

`CHAOS_AGY_HOME` must be a dedicated persistent private directory outside the
source checkout. Chaos preserves OAuth and unrelated top-level settings but
owns the effective MCP server list and permission policy inside this home.
`CHAOS_AGY_PATH` may pin the official CLI binary. Chaos does not wrap provider
login, status, or logout in a separate clamp command namespace. Service
integrations must preserve the Chaos process ID and this home directory, and
must pass the original `-m` model selection again, to resume the same provider
conversation across operating-system processes.
