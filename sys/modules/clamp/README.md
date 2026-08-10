# chaos-clamp

First-party model CLI subprocess transports.

- **Claude Code:** bidirectional stream-JSON control transport. Chaos owns
  tools and permissions through its MCP bridge.
- **Google Antigravity (`agy`):** one subprocess per model turn with explicit
  provider conversation resume and normalized JSONL usage. This transport is
  currently **model-only/sandboxed**: `agy` does not receive permission
  auto-approval, metered Gemini API-key environment variables are removed, and
  full Chaos-owned tool bridging is not yet claimed.

Lifecycle commands:

```bash
CHAOS_AGY_HOME=/private/antigravity-state chaos clamp antigravity connect
CHAOS_AGY_HOME=/private/antigravity-state chaos clamp antigravity status --json
CHAOS_AGY_HOME=/private/antigravity-state chaos clamp antigravity disconnect
```

`CHAOS_AGY_HOME` should be a persistent private directory outside the source
checkout. `CHAOS_AGY_PATH` may pin the official CLI binary. Service integrations
must preserve the Chaos process ID and this home directory, and must pass the
original `-m` model selection again, to resume the same provider conversation
across operating-system processes.
