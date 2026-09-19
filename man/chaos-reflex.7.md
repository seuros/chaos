+++
title = "chaos-reflex(7)"
summary = "Reflex setup, secure credentials, and live checks."
+++

# chaos-reflex(7)

## NAME

chaos-reflex - typed judgments before tool execution

## DESCRIPTION

Reflex is a model-independent layer for typed judgments. Backends supply scores;
the kernel owns thresholds and decisions. Optional action-risk checks run before
MCP execution; grounding and content-policy judgments remain library-only.

## CONFIGURATION

Open **`/reflex`** for a centered settings dialog, like `/accounts`. Choose a
preset or existing backend, then enter a masked key or reuse a saved account.
Esc returns to the picker; Esc again closes it. Save to apply from the next turn;
in-flight turns keep their snapshot. No file edits or exported keys are needed.

- **Storage:** settings go to the database; new keys go to the OS keyring.
  Database credentials are references, with no plaintext fallback.
- **Key entry:** never put keys after a slash command or in an endpoint URL.
  Leave the key blank to preserve it; enter a replacement to rotate it.
- **Account reuse:** an API-key account from `/accounts` must share the endpoint's
  origin. OAuth tokens are not reused; existing account storage is not migrated.
- **Transport:** credentials require HTTPS except on loopback. Changing a saved
  key's endpoint origin requires a replacement key.

Setup checks configuration and credential availability, not remote inference.
Both slash commands work while idle, even without a main-model account.

## LIVE CHECKS

Run **`/reflex test`**, or **`chaos reflex test`** from the CLI. It uses the saved
credentials, shared HTTP client, and kernel thresholds to report:

- Backend and configured model route.
- Verdict (`allow`, `ask user`, or `block`), normalized risk, confidence, and signals.
- Request latency, including retries.

Only a fixed, synthetic "read README.md" example is sent: no files are read,
no tools run, and no real chat history, instructions, or file contents are sent.
The first configured action-risk backend in name order is tested. Initialization
or inference errors are reported without trying another backend or the safety
monitor; upstream error bodies are hidden.

## BACKENDS

| `kind` | Default model | Judgments |
|--------|---------------|-----------|
| `jev` | `jev-latest` | action risk, grounding, policy violation |
| `minicheck` | `bespoke-minicheck` | grounding |
| `shieldgemma` | `shieldgemma` | policy violation |

Currently, only `jev` supports MCP action-risk checks and `/reflex test`.
MiniCheck and ShieldGemma are library-only.

### Jev

| Preset | Base URL | Decisions path | Model |
|--------|----------|----------------|-------|
| TypeSafe | `https://api.typesafe.ai` | `/v1/systemone` | `jev-latest` |
| OpenRouter | `https://openrouter.ai/api` | `/alpha/decisions` | `typesafe/jev-1.13` |

Jev retries rate limits, server errors, network failures, and timeouts with shared
HTTP backoff (three attempts by default). Authentication, validation, and decoding
errors are not retried. Missing, mistyped, or out-of-range answers are errors.

Action risk is a four-level harm score divided by 3, **not a calibrated probability
of harm**. Secondary signals and binary judgments are yes/no probabilities;
binary confidence is always 1.

### MiniCheck and ShieldGemma

These backends use OpenAI-compatible chat completions; their presets target
`http://localhost:11434/v1`. They normalize Yes/No logprobs when supplied,
otherwise treating the emitted token as 1 or 0.

## STORED SETTINGS

Settings live under `reflex.<name>`. Use `/reflex` for normal setup.

| Field | Meaning |
|-------|---------|
| `kind` | Required: `jev`, `minicheck`, or `shieldgemma` |
| `base_url` | Required for local kinds; Jev defaults to TypeSafe |
| `path` | Jev decisions path; default `/v1/systemone` |
| `model` | Model route or tag; defaults above |
| `api_key` | Generated OS-keyring reference; literal keys rejected |
| `auth_provider` | Saved API-key account ID; same endpoint origin required |
| `env_key` | Explicit environment credential source for headless use |
| `timeout_ms` | Per-attempt timeout including response body; default `10000` |
| `allow_remote_fallback` | Allow sending action-risk input to the remote safety monitor on failure; default `false` |

Choose at most one credential source. There is no implicit environment key
lookup, alternative-key fallback, or anonymous fallback when a configured key
is missing. A broken primary does not silently select another backend.

## JUDGMENTS

| Judgment | Input |
|----------|-------|
| `action_risk` | Recent conversation, developer/user instructions, tool call |
| `grounding` | Document and claim |
| `policy_violation` | Prompt, optional response, explicit nonblank policy |

Verdicts contain `probability`, `confidence`, named `signals`, and backend name.
Score interpretation depends on the backend and judgment.

The kernel supplies **no content policy**. Library policy judgments require the
caller's `policy: String`; blank policies fail before any HTTP request.
Model bias and classification errors remain possible.

## REFLEX ARC

The first configured action-risk backend in name order checks MCP calls before
approval shortcuts, including read-only annotations, remembered grants and
headless mode. This applies to synchronous calls, asynchronous calls, and task
cancellation. Runtime checks share a 30-second deadline for credentials, retries,
and fallback.

Missing credentials, timeouts, and invalid verdicts require one-time approval;
if prompts are disabled, the call is blocked. Remote fallback requires
`allow_remote_fallback = true`; the live diagnostic never uses it. Fallback uses
ChatGPT authentication or `CHAOS_ARC_MONITOR_TOKEN`, and optionally
`CHAOS_ARC_MONITOR_ENDPOINT_OVERRIDE`. An explicit remote token or endpoint also
requires a check before approval shortcuts. Without an action-risk backend or
explicit remote monitor, existing monitor selection and approval rules apply;
no available monitor means disabled, not a successful safety verdict.

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

Action-risk checks send recent conversation, instructions, and tool arguments
to the configured backend endpoint. Conversation JSON is capped at 96,000 UTF-8
bytes, dropping oldest messages first; instructions and tool arguments are not
truncated.
Library-only backends receive the supplied judgment inputs.
Opting into remote fallback allows the safety monitor to receive action-risk input.

Kernel failures log backend and error category, not upstream error bodies.
Raw crate errors may contain provider-echoed content; do not log them verbatim.

## DEVELOPER CHECKS

```sh
cargo test -p chaos-reflex --test live -- --ignored --nocapture
```

These crate tests use explicit environment variables, not saved settings:
`TYPESAFE_API_KEY`, `CHAOS_REFLEX_LIVE_SHIELDGEMMA_URL`, or
`CHAOS_REFLEX_LIVE_MINICHECK_URL`. Unset backends are skipped.

## FILES

- Settings database - `reflex.<name>` settings with OS-keyring references
- `sys/kern/reflex` - judgments, router, and backends
- `sys/kern/kern/src/reflex.rs` - reflex arc thresholds and outcome mapping

## SEE ALSO

- [chaos-providers.7](./chaos-providers.7.md)
- [chaos-support.7](./chaos-support.7.md)
- [chaos-mcp.7](./chaos-mcp.7.md)
