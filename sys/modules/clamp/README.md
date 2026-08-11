# chaos-clamp

First-party model CLI subprocess transports.

- **Claude Code:** bidirectional stream-JSON control transport. Chaos owns
  tools and permissions through its MCP bridge.
- **Google Antigravity (`agy`):** one subprocess per model turn with explicit
  provider conversation resume and normalized JSONL usage. `agy` runs
  sandboxed with native command, filesystem, unsandboxed, and URL operations
  denied. Its sole action surface is the session-scoped Chaos MCP bridge.
  Metered Gemini API-key environment variables are removed, and the ephemeral
  bridge socket/token are inherited by the MCP child without being written to
  persistent configuration.

Lifecycle commands:

```bash
CHAOS_AGY_HOME=/private/antigravity-state chaos clamp antigravity connect
CHAOS_AGY_HOME=/private/antigravity-state chaos clamp antigravity status --json
CHAOS_AGY_HOME=/private/antigravity-state chaos clamp antigravity disconnect
```

`CHAOS_AGY_HOME` must be a dedicated persistent private directory outside the
source checkout. Chaos preserves OAuth and unrelated top-level settings but
owns the effective MCP server list and permission policy inside this home.
`CHAOS_AGY_PATH` may pin the official CLI binary. Service integrations must
preserve the Chaos process ID and this home directory, and must pass the
original `-m` model selection again, to resume the same provider conversation
across operating-system processes.
