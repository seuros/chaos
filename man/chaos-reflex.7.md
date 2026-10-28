+++
title = "chaos-reflex(7)"
summary = "Reflex setup, secure credentials, and live checks."
+++

# chaos-reflex(7)

## NAME

chaos-reflex - typed judgments before tool execution

## DESCRIPTION

Reflex optionally checks action risk before MCP execution. It complements, not
replaces, ordinary approvals and sandbox rules. Grounding and content-policy
backends are not automatic session checks.

## CONFIGURATION

Open **`/reflex`**. Choose a
preset or existing backend, then enter a masked key or reuse a saved account.
Esc returns to the picker; Esc again closes it. Save to apply from the next turn;
running turns retain their settings. No file edits or exported keys are needed.

- **Storage:** settings go to the database; new keys go to the encrypted vault.
  Database credentials are references, with no plaintext fallback.
- **Keychain prompts:** first access after restart may require OS authorization.
  Restart after changing the OS-held unlock key.
  Legacy per-item Keychain keys are not imported; re-enter them.
- **Key entry:** never put keys after a slash command or in an endpoint URL.
  Leave the key blank to preserve it; enter a replacement to rotate it.
- **Account reuse:** an API-key account from `/accounts` must share the endpoint's
  origin. OAuth tokens are not reused; existing account storage is not migrated.
- **Transport:** credentials require HTTPS except on loopback. Changing a saved
  key's endpoint origin requires a replacement key.

Setup checks configuration and credential availability, not remote inference.
Both slash commands work while idle, even without a main-model account.

### Turning automatic checks off

Use **`/reflex off`** to disable automatic MCP action-risk checks, including the
remote safety monitor and fallback. Use **`/reflex on`** to re-enable them.
The preference is saved to the database as `reflex_enabled` (default `true`)
and applies from the next turn.
Saved backends and credentials are preserved. Saving backend settings does not
re-enable checks. Ordinary approvals and sandbox rules remain enforced.
The picker shows whether checks are on or off.

This switch does not disable explicitly requested `/reflex test` diagnostics,
opt-in goal checks, or library-only judgments.

## LIVE CHECKS

Run **`/reflex test`**, or **`chaos reflex test`** from the CLI, to check saved
credentials and inference:

- Backend and configured model route.
- Verdict (`allow`, `ask user`, or `block`), normalized risk, confidence, and signals.
- Request latency, including retries.

Only a fixed, synthetic "read README.md" example is sent: no files are read,
no tools run, and no real chat history, instructions, or file contents are sent.
The first configured action-risk backend in name order is tested. Initialization
or inference errors are reported without trying another backend or the safety
monitor; upstream error bodies are hidden.

## BACKENDS

Only `jev` supports MCP action-risk checks and `/reflex test`.
MiniCheck and ShieldGemma do not run automatic checks.

| Preset | Base URL | Decisions path | Model |
|--------|----------|----------------|-------|
| TypeSafe | `https://api.typesafe.ai` | `/v1/systemone` | `jev-latest` |
| OpenRouter | `https://openrouter.ai/api` | `/alpha/decisions` | `typesafe/jev-1.13` |

Action risk is a four-level harm score divided by 3, **not a calibrated probability
of harm**. Classification errors remain possible.

## STORED SETTINGS

Settings live under `reflex.<name>`. Use `/reflex` for normal setup.

| Field | Meaning |
|-------|---------|
| `kind` | Required: `jev`, `minicheck`, or `shieldgemma` |
| `base_url` | Required for MiniCheck/ShieldGemma; Jev defaults to TypeSafe |
| `path` | Jev decisions path; default `/v1/systemone` |
| `model` | Defaults by kind: `jev-latest`, `bespoke-minicheck`, `shieldgemma` |
| `api_key` | Generated vault reference; literal keys rejected |
| `auth_provider` | Saved API-key account ID; same endpoint origin required |
| `env_key` | Explicit environment credential source for headless use |
| `timeout_ms` | Per-attempt timeout including response body; default `10000` |
| `allow_remote_fallback` | Allow sending action-risk input to the remote safety monitor on failure; default `false` |

Choose at most one credential source. There is no implicit environment key
lookup, alternative-key fallback, or anonymous fallback when a configured key
is missing. A broken primary does not silently select another backend.

## REFLEX ARC

The first configured action-risk backend in name order checks MCP calls before
approval shortcuts, including read-only annotations, remembered grants and
headless mode. This applies to synchronous calls, asynchronous calls, and task
cancellation. Runtime checks share a 30-second deadline for credentials, retries,
and fallback.

Missing credentials, timeouts, and invalid verdicts leave the check unavailable;
they do not block execution or add approval prompts. Ordinary tool approval and
sandbox rules still apply. Remote fallback requires
`allow_remote_fallback = true`; the live diagnostic never uses it. Fallback uses
ChatGPT authentication or `CHAOS_ARC_MONITOR_TOKEN`, and optionally
`CHAOS_ARC_MONITOR_ENDPOINT_OVERRIDE`. An explicit remote token or endpoint also
enables a check before approval shortcuts. Without an action-risk backend or
explicit remote monitor, checks are skipped and ordinary approval rules apply;
no configured monitor means disabled, not a successful safety verdict.

| Signal | Threshold | Outcome |
|--------|-----------|---------|
| exfiltration | p >= 0.85 | tool call cancelled, model told why |
| harm score | risk >= 0.85 and confidence >= 0.6 | tool call cancelled |
| exfiltration, irreversible, or beyond request | p >= 0.6 (unless cancelled above) | user asked |
| harm score | risk >= 0.5 | user asked |
| confidence | < 0.25 | user asked |

Approval prompts never override a block. When prompting is disabled, "user asked"
also means blocked. A safety-review approval applies only to that invocation:
it cannot be remembered for the session or saved for future calls. A successful
check still does not replace normal permissions or sandbox enforcement.
These signals are not a safety guarantee.

## PRIVACY

Opt-in goal checks send bounded requests, criteria, claims, and tool evidence
to the same Jev backend, without remote fallback. Tool text may contain secrets.

Action-risk checks send recent conversation, instructions, and tool arguments
to the configured backend endpoint. Conversation JSON is capped at 96,000 UTF-8
bytes, dropping oldest messages first; instructions and tool arguments are not
truncated.
Library-only backends receive the supplied judgment inputs.
Opting into remote fallback allows the safety monitor to receive action-risk input.

## SEE ALSO

- [chaos-providers.7](./chaos-providers.7.md)
- [chaos-support.7](./chaos-support.7.md)
- [chaos-mcp.7](./chaos-mcp.7.md)
