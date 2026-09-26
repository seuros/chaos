+++
title = "chaos-providers(7)"
summary = "Provider configuration and model selection."
+++

# chaos-providers(7)

## NAME

chaos-providers - configure and select LLM providers for FreeChaOS

## DESCRIPTION

FreeChaOS is provider-agnostic. The kernel speaks the Chaos-ABI; adapters
translate that to whatever wire format a given provider expects. New
providers are added through `~/.chaos/config.toml` - no code changes, no
rebuilds.

For the condensed support matrix (bundled IDs, wire formats, OS/CI coverage, clamp backends), see [chaos-support(7)](./chaos-support.7.md).

## GLOBAL EGRESS

Route all model providers through LLM Service Daemon (LSD):

```sh
export CHAOS_EGRESS_URL="http://localhost:8847/egress/chaos"
```

This overrides root-level `egress_url` in `~/.chaos/config.toml`.
Keep provider URLs and credentials unchanged. Invalid or empty values fail
configuration loading. There is no direct fallback; LSD rejects local/private
providers. Leave both settings unset for direct access.

Use a trusted gateway: it receives credentials and prompts. See LSD's
documentation for gateway setup and policy.

## BUILT-IN PROVIDERS

FreeChaOS ships with these providers preconfigured:

| Provider | Wire format | Notes |
|----------|-------------|-------|
| `openai` | Responses API | Default, hardcoded |
| `anthropic` | Anthropic Messages | Hardcoded; URL-detected |
| `xai` | Responses API | Bundled; native `web_search` / `x_search` tools |
| `moonshotai` | Responses API | Bundled; Moonshot AI pay-per-token API |
| `moonshotai-coding` | Responses API | Bundled; Kimi Code subscription |
| `zai` | Chat Completions | Bundled; Z.ai pay-per-token (GLM-5 / GLM-5.1) |
| `zai-coding` | Chat Completions | Bundled; Z.ai GLM Coding Plan subscription |
| `charm` | Chat Completions | Bundled via `thirdparty.toml` |

Any other provider — DeepSeek, Groq, Ollama, MiniMax, LSD,
self-hosted gateways — is a config entry away.

### API-key validation

`/accounts` and `chaos accounts --with-api-key` validate keys in the kernel
before saving credentials. A format mismatch leaves existing credentials
unchanged and does not switch providers.

The harness has internal format rules for `openai`, `anthropic`, `charm`,
`moonshotai`, `moonshotai-coding`, and `xai`. For example, an OpenAI
`sk-proj-…` key cannot be saved for `anthropic`, and a Kimi Code `sk-kimi-…`
key cannot be saved for the pay-per-token `moonshotai` account. Custom
provider IDs have no vendor-specific format rules. All keys must be nonempty,
printable ASCII without embedded whitespace; surrounding pasted whitespace
is stripped on save.

These checks are local format validation, not remote authentication. They
cannot prove a key is active or distinguish providers sharing the same format
(such as legacy OpenAI and Moonshot `sk-…` keys). Prefix rules are not
user-configurable.

## CLAMP TRANSPORTS

Clamp uses an installed, authenticated first-party CLI with Chaos tools over MCP.
Claude Code is the default; Antigravity (`agy`) is experimental. CLI, authentication,
or bridge failures never fall back to metered API billing.

- `/clamp claude` or `/clamp agy` selects a backend (`claude-code` and `antigravity`
  are aliases).
- Bare `/clamp` toggles the configured or last-selected backend; `/clamp off`
  restores the original API model and reasoning settings.
- Switching is session-local, disabled during turns, and needs no API account.
- Start clamped with `--clamp` or `-c clamp=true`; select AGY with
  `-c clamp_backend=antigravity`.

With AGY, `/model` accepts a slug from `agy models`. `antigravity.model` or
`CHAOS_AGY_MODEL` pins the model until a new session; otherwise activation keeps
a compatible current model or uses `gemini-3.1-pro-low`.

Both backends receive the same Chaos base instructions used by direct API
requests. Claude Code receives them through `--system-prompt-file` at subprocess
startup. AGY's TLS-inspecting egress proxy replaces the CLI-generated
`systemInstruction` in supported Cloud Code and Gemini JSON generation requests.
Both `cloudcode-pa.googleapis.com` and `daily-cloudcode-pa.googleapis.com` use
this replacement for `v1internal:generateContent` and
`v1internal:streamGenerateContent`. Allowing a host never bypasses prompt ownership.
The canonical prompt is refreshed before every turn, including resumes; empty
instructions remove the CLI's system instructions. No custom AGY agent or
user-message prompt injection is used. Encoded, malformed, cached-content, and
unsupported generation requests fail rather than falling back to the CLI prompt.
Clamp serializes history as text when bootstrapping a new provider conversation.
Compatible Claude Code resumes retain the CLI's native assistant/tool history
and append only the new Chaos input, including new hook/developer messages.

### Antigravity setup

Use a dedicated private home: Chaos replaces its MCP and permission configuration
and denies native AGY tools. Missing or empty (ASCII-whitespace-only) managed
configuration files are initialized; malformed JSON and non-object values are
rejected rather than overwritten. Authenticate through the official CLI using that home:

```bash
export CHAOS_AGY_HOME=/private/antigravity-state
export CHAOS_AGY_PATH=/opt/antigravity/bin/agy # optional if agy is on PATH
mkdir -p "$CHAOS_AGY_HOME"
chmod 700 "$CHAOS_AGY_HOME"
env -u GEMINI_API_KEY -u GOOGLE_API_KEY \
  HOME="$CHAOS_AGY_HOME" XDG_CONFIG_HOME="$CHAOS_AGY_HOME/.config" \
  "${CHAOS_AGY_PATH:-agy}" models
```

The CLI owns login, refresh, account selection, and logout. Keep its home on private
persistent storage for hosted workers; never put browser authorization codes in
config or logs. Chaos removes metered API keys from AGY's environment.
Using the official CLI does not guarantee provider approval; verify that your
subscription terms permit this use.

### Headless sessions and resume

```bash
chaos exec --json -c clamp=true -m claude-sonnet-4-5 "say ok"
chaos exec --json -c clamp=true -c clamp_backend=antigravity \
  -m gemini-3.1-pro-low "say ok"
chaos exec --json -c clamp=true -c clamp_backend=antigravity \
  -m gemini-3.1-pro-low resume <process_id> "continue"
```

Resumes require the same effective clamp configuration, including any non-default
backend. For AGY, retain the process ID, dedicated home, CLI path, and model
selection across invocations and workers.

Claude Code resumes also require its native session files (normally under
`~/.claude/projects`) and the same working directory to survive between
invocations. Chaos stores owner-only, per-process checkpoints under
`$CHAOS_HOME/clamp/claude`; these contain a native session ID and history
fingerprints, not transcript contents. Ephemeral Chaos sessions keep checkpoints
only in memory.

A checkpoint is reused only when the model, base instructions, working directory,
and completed Chaos history prefix still match. New/forked sessions, rewritten
history, changed instructions, and absent checkpoints bootstrap from the current
Chaos transcript instead. Checkpoints are consumed before dispatch and renewed
only after successful completion. A missing native session or failed resume
surfaces an error, rather than automatically replaying a potentially
side-effecting turn; the next attempt bootstraps without that stale checkpoint.
Native resume enables prefix reuse but does not guarantee a cache hit: cache
expiry and provider/tool-prefix changes still apply.

## SESSION PICKER

The resume and fork pickers show sessions across providers. When the selected
session uses another provider, opening it automatically restores that provider
and its saved model. A notice explains the switch; press **Tab** before opening
to keep the current model instead, or Tab again to restore automatic switching.
The override is per session within the picker. **Shift+Tab** changes the sort order.
This does not change your saved provider defaults. Missing providers or saved
model metadata produce an error rather than silently using another model.

## WIRE FORMATS

Providers speak one of four wire formats:

- **Responses API** — OpenAI's `/v1/responses`. What OpenAI ships and most imitators clone.
- **Chat Completions** — `/v1/chat/completions`. The lingua franca of OpenAI-compatible gateways.
- **Anthropic Messages** — Anthropic's native format. Auto-detected when the base URL contains `anthropic`.
- **TensorZero** — TensorZero's native `/inference` endpoint. Opt in with `wire_api = "tensorzero"`.

By default, providers use `wire_api = "auto"` — FreeChaOS tries Responses
first and falls back to Chat Completions on 404/405/501. The winning
format is cached for the session.

You add a `[model_providers.<id>]` block, set your API key in the
environment, and go.

## EXAMPLES

### OpenAI ChatGPT context window

For GPT-5.6 Sol, Chaos uses the context window advertised by the provider
catalog by default:

```toml
chatgpt_context_window = "catalog"
```

The ChatGPT OAuth route has also been observed accepting more context than its
catalog advertises. Users who prefer the experimentally observed window can
opt in:

```toml
chatgpt_context_window = "observed-400k"
```

You can also persist either choice from the TUI:

```text
/context-window catalog
/context-window observed-400k
```

Run `/context-window` or `/context-window status` to show the active preset.
Changes made through the command apply to newly started Chaos sessions.

For `gpt-5.6-sol` on the built-in OpenAI provider, this selects a 400,000-token
window and automatic compaction at 350,000 tokens. It does not change other
models or providers. The provider may change its accepted limit independently
of Chaos, so `catalog` remains the default.

The lower-level numeric settings remain available for advanced use:

```toml
model_context_window = 400000
model_auto_compact_token_limit = 350000
```

An explicit `model_context_window` disables the named preset. An explicit
`model_auto_compact_token_limit` overrides the preset's 350,000-token
compaction point.

Before automatic compaction, Chaos uses the reserve between the compaction
threshold and the hard context window to inject a once-per-window,
model-visible continuity reflex. The agent receives the notice in ordinary
conversation history and sees it on the next normal model iteration with its
usual tools, allowing it to perform its configured journaling, memory, or
operational continuity practices. If token usage has already crossed the soft
compaction threshold while remaining below the 90% safety ceiling, Chaos grants
one immediate iteration before compacting. The reflex is directed to the agent
rather than emitted as a user-facing compaction warning.

Users can optionally give the agent bounded timing control:

```toml
agent_compaction_control = "bounded"
```

The default is `"disabled"`. In bounded mode, Chaos exposes a
`compaction_control` tool. The agent may request immediate compaction at the
next safe turn-loop boundary, or defer the current pressure window once after
receiving its reflex. A deferral cannot stack or change its own limits: Chaos
keeps an absolute ceiling equal to the smaller of 90% of the raw model window
and the effective input window minus a 20,000-token distillation reserve.
For the `observed-400k` preset, that ceiling is 360,000 tokens, so deferral
extends the normal 350,000-token threshold by only 10,000 tokens. Models whose
catalog window is reduced to an 80% effective input window can have a larger
usable band.
Doing nothing retains normal automatic compaction. Accepted decisions are
persisted for resume continuity; forks inherit transcript history but not a
pending decision made by the source agent.

The user-facing term *compaction* corresponds to the kernel's internal
*distillation* implementation.

### Terminal session titles

Chaos can mirror the active session's durable process name into the terminal
title. This lets terminals such as WezTerm display descriptive tab labels:

```toml
# Never emit terminal-title changes.
terminal_title = "off"

# Mirror names set with /rename or restored on resume. This is the default.
terminal_title = "process-name"

# Also let the current root agent name the session.
terminal_title = "agent"

# Optional identity and working markers are TUI presentation only.
[tui]
terminal_title_icon = "✦"
terminal_title_working_icon = "◒"
```

The idle icon prefixes both named sessions and the `new session` fallback. While
a model turn or MCP startup is active, the working icon replaces it. If only
`terminal_title_icon` is configured, that icon remains visible in both states.
Icons must be one grapheme cluster and no more than four terminal cells; an
empty string disables the corresponding icon.

Agent mode exposes `set_session_title` to the current root agent and privately
asks it to review the title at the start of a session, after resume or
compaction, and periodically during longer work. A reminder does not require a
rename: the agent is instructed to retain a title that still accurately names
the session's primary work. Reconnecting also runs a hidden, title-only review
against the restored conversation before accepting a new user turn, allowing an
unnamed or stale tab to update immediately. The title remains the existing
process name used by `/rename` and `chaos resume`; the separate database `title`
field remains a first-message preview and is never used as the terminal
fallback. An explicit `/rename` always marks the name as user-authored,
preventing the agent from replacing it. Agent-generated names must also be
distinct from other unarchived sessions.

Chaos emits the portable OSC terminal-title sequence and clears it when the TUI
exits. Terminal configuration can still override or transform the displayed
tab title; for example, a custom WezTerm `format-tab-title` callback may choose
not to show the pane title. OSC titles cannot independently color the icon, but
WezTerm can style configured markers locally:

```lua
local wezterm = require("wezterm")

wezterm.on("format-tab-title", function(tab)
  local title = tab.active_pane.title
  local color = title:match("^✦ ") and "#7bd88f"
    or title:match("^◒ ") and "#f5c451"
  if color then
    return {
      { Foreground = { Color = color } },
      { Text = title },
    }
  end
  return title
end)
```

### xAI (Grok)

Bundled — no config needed. Just export the key:

```bash
export XAI_API_KEY=xai-...
chaos --provider xai --model grok-4
```

URLs containing `x.ai` automatically expose xAI's native `web_search` and
`x_search` server-side tools — no function schema needed. Override the
bundled config by redeclaring `[model_providers.xai]` in your
`~/.chaos/config.toml`.

Provider HTTP 402 (Payment Required) responses, including Grok Build balance
exhaustion, are reported as a non-retryable quota error (`UsageLimitExceeded`
in protocol events). The harness can handle the limit without repeated
requests to the exhausted account; replenish the balance or switch providers
before retrying.

### Anthropic (Claude)

Already built-in, but you can override its config:

```toml
[model_providers.anthropic]
name = "Anthropic"
base_url = "https://api.anthropic.com/v1"
env_key = "ANTHROPIC_API_KEY"
```

```bash
export ANTHROPIC_API_KEY=sk-ant-...
chaos --provider anthropic --model haiku
```

The URL contains `anthropic`, so FreeChaOS routes to the Messages API adapter.
For the official Anthropic endpoint, five-minute prompt caching is enabled by
default. Set `CHAOS_ANTHROPIC_CACHE_TTL=1h` for the extended TTL or
`CHAOS_ANTHROPIC_CACHE_TTL=off` to disable caching. A request-level
`anthropic_cache_ttl` extension (`5m`, `1h`, or `off`) takes precedence over
the environment variable.

### TensorZero

```toml
[model_providers.tensorzero]
name = "TensorZero"
base_url = "http://localhost:3000"
wire_api = "tensorzero"
```

```bash
chaos --provider tensorzero --model my-function
```

TensorZero has its own inference protocol — explicit `wire_api` required.

### Z.ai (GLM)

Bundled as two separate providers — Z.ai runs distinct endpoints for
pay-per-token API access and the GLM Coding Plan subscription. Both
share the same `ZAI_API_KEY` env var; pick the provider that matches
your billing.

Pay-per-token (standard API):

```bash
export ZAI_API_KEY=your-key
chaos --provider zai --model glm-5.1
```

GLM Coding Plan (subscription):

```bash
export ZAI_API_KEY=your-key
chaos --provider zai-coding --model glm-5.1
```

Z.ai is the international brand for ZhipuAI's GLM models (GLM-5, GLM-5.1).

### Moonshot AI (Kimi)

Bundled as two providers with separate endpoints and credentials. No custom
provider configuration is required.

In the TUI, open `/accounts` and choose **Moonshot AI** for the pay-per-token
API or **Moonshot AI Coding** for a Kimi Code subscription. Both open API-key
entry with the corresponding key-creation instructions. Paste the key and
press Enter to save it in the configured credential store; environment
variables are not required. The two providers keep separate credentials.

Pay-per-token API:

```bash
export MOONSHOT_API_KEY=your-platform-key
chaos --provider moonshotai --model kimi-k3
```

Kimi Code subscription:

```bash
export KIMI_API_KEY=your-coding-key
chaos --provider moonshotai-coding --model kimi-for-coding
```

`moonshotai` uses `https://api.moonshot.ai/v1`; `moonshotai-coding` uses the international
Kimi Code endpoint, `https://api.kimi.ai/coding/v1`. Create the corresponding
key in the Kimi API Platform or Kimi Code console. These are separate billing
routes: Chaos never falls back from the subscription to the metered API.

Both use HTTP/SSE Responses requests, including function tools and native
`web_search`. Kimi's plaintext reasoning history is preserved across tool calls;
OpenAI encrypted reasoning, summary controls, and service tiers are not sent.
Use `low`, `high`, or `max` effort; `none`/`minimal` map to `low`, `medium` to
`high`, and `xhigh`/`ultra` to `max`. Kimi does not support disabling reasoning
with `none`; Chaos sends its lowest supported effort instead. An unspecified
effort is left unspecified.

The pay-per-token Responses endpoint currently supports `kimi-k3`; other
models advertised by `/models` may require a different wire API. Kimi Code
model access depends on your subscription. Refresh the catalog with:

```bash
chaos --provider moonshotai models --refresh
chaos --provider moonshotai-coding models --refresh
```

Context windows and image/thinking capabilities are taken from the provider's
model catalog, not guessed from model names. For a regional endpoint, override
the bundled provider in `~/.chaos/config.toml` and use a key for that region.
For example, the China Kimi Code endpoint is:

```toml
[model_providers.moonshotai-coding]
name = "Moonshot AI Coding"
model_family = "kimi"
base_url = "https://api.kimi.com/coding/v1"
env_key = "KIMI_API_KEY"
wire_api = "responses"
native_server_side_tools = ["web_search"]
```

Protocol references:
`https://platform.kimi.ai/docs/api/responses` and
`https://www.kimi.com/code/docs/en/third-party-tools/codex.html`.

### DeepSeek

```toml
[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
env_key = "DEEPSEEK_API_KEY"
```

### Groq

```toml
[model_providers.groq]
name = "Groq"
base_url = "https://api.groq.com/openai/v1"
env_key = "GROQ_API_KEY"
```

### Ollama (local)

```toml
[model_providers.ollama]
name = "Ollama"
base_url = "http://localhost:11434/v1"
```

No `env_key` needed — Ollama runs locally without authentication.

```bash
chaos --provider ollama --model llama3
```

### Anthropic-compatible proxies (MiniMax, Kimi, Z.ai)

Any provider whose base URL contains `anthropic` gets routed to the
Anthropic Messages adapter:

```toml
[model_providers.minimax]
name = "MiniMax"
base_url = "https://api.minimax.io/anthropic"
env_key = "MINIMAX_API_KEY"
```

Prompt caching remains opt-in for compatible endpoints because some providers
reject Anthropic-specific request fields. Set `CHAOS_ANTHROPIC_CACHE_TTL=5m`
or `1h` only when the endpoint supports top-level `cache_control`.

## REFLEX

Use `/reflex` to configure judgment backends and save keys securely.
Test the configured action-risk backend with `/reflex test` or `chaos reflex test`.
See [chaos-reflex(7)](./chaos-reflex.7.md).

## CONFIGURATION

| Field | Required | Description |
|-------|----------|-------------|
| `name` | yes | Display name for logs and model selection |
| `base_url` | yes | Provider API endpoint |
| `env_key` | no | Environment variable holding the API key |
| `env_key_instructions` | no | Help text shown when the key is missing |
| `wire_api` | no | `"auto"` (default), `"responses"`, `"chat_completions"`, or `"tensorzero"`. Anthropic is URL-detected and overrides this. |
| `http_headers` | no | Static headers as `{ "Header-Name" = "value" }` |
| `env_http_headers` | no | Headers from env vars as `{ "Header-Name" = "ENV_VAR" }` |
| `query_params` | no | Query string parameters as `{ "key" = "value" }` |
| `request_max_retries` | no | HTTP retry limit (default: 4, max: 100) |
| `stream_max_retries` | no | Stream reconnect limit (default: 5, max: 100) |
| `stream_idle_timeout_ms` | no | Idle timeout in ms (default: 300000) |
| `supports_websockets` | no | Enable WebSocket transport (default: false) |
| `experimental_bearer_token` | no | Hardcoded bearer token (discouraged — use `env_key`) |

## SELECTION RULES

FreeChaOS resolves the wire format in this order:

1. If the `base_url` contains `anthropic` → Anthropic Messages API (overrides `wire_api`)
2. If `wire_api` is set explicitly → use it (`responses`, `chat_completions`, `tensorzero`)
3. Otherwise → `auto`: try Responses, fall back to Chat Completions on 404/405/501

There is no `wire_api = "anthropic"` option. The URL is the signal.

## TROUBLESHOOTING

**"env var not set"** — Export the API key variable listed in `env_key`.

**Provider returns errors** — Check that `base_url` points to the correct
API version endpoint. Most OpenAI-compatible providers use `/v1`.

**Timeouts on slow providers** — Increase `stream_idle_timeout_ms`:

```toml
[model_providers.slow]
stream_idle_timeout_ms = 600000
```

**Rate limiting** — FreeChaOS retries 429s automatically. Increase
`request_max_retries` if needed.

## FILES

- `~/.chaos/config.toml` - user-level provider configuration
- `thirdparty.toml` - bundled provider definitions referenced by the tree

## SEE ALSO

- [chaos-install.7](./chaos-install.7.md)
- [chaos-reflex.7](./chaos-reflex.7.md)
- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-halluacinate.7](./chaos-halluacinate.7.md)

### Antigravity tool catalogue

The canonical Antigravity system prompt includes the current Chaos MCP tool
catalogue. It uses the same function schemas and freeform `input` envelope as
the session bridge; provider-native tool declarations are excluded. The catalogue
is regenerated from each sampling turn's tools, so removed tools are not retained
in the replacement system prompt. Tools remain subject to Chaos permissions.

### Model discovery with CLI clamp

CLI-backed (`clamp = true`) sessions use fresh, version-compatible cached model
metadata instead of automatic native API discovery, including Anthropic metadata
lookups and ETag-triggered refreshes. Cold or stale caches do not trigger native
authentication; uncached model metadata falls back to the normal descriptor.
An already loaded catalog remains available in memory.

Explicit `refresh_models` / `models --refresh` requests still perform native
discovery for the requested provider/account and report missing credentials or
discovery failures. A CLI login alone is not a native API credential. Custom
authoritative catalogs remain unchanged.
