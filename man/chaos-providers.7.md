+++
title = "chaos-providers(7)"
summary = "Provider configuration and model selection."
+++

# chaos-providers(7)

## NAME

chaos-providers - configure and select LLM providers for FreeChaOS

## DESCRIPTION

Each session selects a provider ID and model. Provider settings control the
endpoint, credentials, wire format, and transport. Additional providers use
`[model_providers.<id>]` entries in the persisted configuration.

See [chaos-support(7)](./chaos-support.7.md) for available providers and formats,
and [chaos-storage(7)](./chaos-storage.7.md) for configuration storage.

## GLOBAL EGRESS

Route all model providers through LLM Service Daemon (LSD):

```sh
export CHAOS_EGRESS_URL="http://localhost:8847/egress/chaos"
```

This overrides root-level `egress_url` in `~/.chaos/config.toml`.
Keep provider URLs and credentials unchanged. Invalid or empty values fail
configuration loading. LSD rejects local/private providers; gateway failures
are reported as errors. Leave both settings unset for direct access.

Use a trusted gateway: it receives credentials and prompts. See LSD's
documentation for gateway setup and policy.

## BUILT-IN PROVIDERS

FreeChaOS ships with these providers preconfigured:

| Provider | Wire format | Notes |
|----------|-------------|-------|
| `openai` | Responses API | Default provider |
| `anthropic` | Anthropic Messages | Native Anthropic API |
| `xai` | Responses API | Native `web_search` / `x_search` tools |
| `moonshotai` | Responses API | Moonshot AI pay-per-token API |
| `moonshotai-coding` | Responses API | Kimi Code subscription |
| `zai` | Chat Completions | Z.ai pay-per-token (GLM-5 / GLM-5.1) |
| `zai-coding` | Chat Completions | Z.ai GLM Coding Plan subscription |
| `charm` | Chat Completions | Charm API |

Additional providers — DeepSeek, Groq, Ollama, LSD,
self-hosted gateways — is a config entry away.

### API-key validation

`/accounts` and `chaos accounts --with-api-key` validate keys in the kernel
before saving credentials. A format mismatch leaves existing credentials
unchanged and does not switch providers.

Provider-specific key formats are checked for `openai`, `anthropic`, `charm`,
`moonshotai`, `moonshotai-coding`, and `xai`. For example, an OpenAI
`sk-proj-…` key cannot be saved for `anthropic`, and a Kimi Code `sk-kimi-…`
key cannot be saved for the pay-per-token `moonshotai` account. Custom
provider IDs have no vendor-specific format rules. All keys must be nonempty,
printable ASCII without embedded whitespace; surrounding pasted whitespace
is stripped on save.

These checks are local format validation, not remote authentication. They
cannot prove a key is active or distinguish providers sharing the same format
(such as legacy OpenAI and Moonshot `sk-…` keys).

### Responses transport

```sh
CHAOS_OPENAI_WEBSOCKET=0 chaos # Explicitly disable WS and use HTTP/SSE
```

OpenAI uses Responses WebSocket v2 by default. Other providers use it only
when `supports_websockets = true` and `wire_api` is `auto` or `responses`.
Set `supports_websockets = false` to disable it for a provider, or use
`CHAOS_OPENAI_WEBSOCKET=0` to disable it globally.

Azure and providers routed through global egress use HTTP/SSE. Clamped
sessions use the selected CLI transport.

WebSocket failures terminate the request with an error.

### ChatGPT subscription web search

The official ChatGPT subscription Responses endpoint supports text web search,
but rejects image search even when its model catalog advertises
`text_and_image`. The OpenAI adapter uses text-only web search for that endpoint
on both HTTP and WebSocket transports. Search filters, location, context size,
and live/cached access settings are preserved.

The subscription WebSocket endpoint also rejects search added after a socket's
first request initialized without it. When native web search is activated later
(for example by `enable_tools`), Chaos opens a fresh authenticated socket before
sending that request, retaining the full conversation input. Unused prewarmed
sockets and sockets initialized with search remain reusable. This does not retry
a failed request or fall back to HTTP. To use HTTP/SSE explicitly while retaining
search, launch with `CHAOS_OPENAI_WEBSOCKET=0 chaos`.

The provider catalog is not modified or pinned. Public OpenAI API, Azure,
custom endpoints, and other providers retain their declared search capabilities.
This compatibility rule does not disable image input or image generation.

## CLAMP TRANSPORTS

Clamp uses an installed, authenticated first-party CLI with Chaos tools over MCP.
Claude Code is the default; Antigravity (`agy`) is experimental. CLI, authentication,
or bridge failures terminate the turn with an error.

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

Both backends use the session's Chaos instructions. Guidance is refreshed
before every turn, including resumes; CLI-provided instructions do not replace
the session's instructions.

New provider conversations receive the current Chaos transcript as text.
Compatible resumes retain the native conversation and receive new Chaos
input, including hook and developer messages.

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

### Sub-agents under clamp

A sub-agent spawned or resumed by a clamped parent inherits the parent's live
transport, including a `/clamp` toggle made during the session. When the child
resolves to a different provider (through an explicit provider or a role), it
uses that provider's own transport and is never routed through the parent's CLI.

With Claude Code, `spawn_agent` validates a requested `model` and
`reasoning_effort` against the models the CLI advertised to the parent's
session (for example `haiku`, `sonnet`, `opus`), and passes the model to
the child session. No Anthropic API key is needed. Unknown models and
effort levels the CLI did not advertise are rejected rather than substituted.

### Continuation behavior

Claude Code resumes also require its native session files (normally under
`~/.claude/projects`) and the same working directory to survive between
invocations.

Request-local runtime guidance and machine warnings are sent on every request,
without invalidating an otherwise compatible native conversation.

Native continuation requires a matching model, instructions, working directory,
and completed history prefix. New or forked sessions, rewritten or compacted
history, and changed context start from the current Chaos transcript.

A missing native session or failed resume is reported as an error. Antigravity
transport errors are terminal for that turn and do not automatically replay
tool actions. A subsequent explicit attempt starts a new provider conversation
when no compatible completed resume state is available.

Failure does not roll back side effects. Inspect the session history before
retrying an interrupted turn. For Antigravity, keep the configured conversation
directory and native CLI history together.

### Antigravity tools

Antigravity receives the current session's Chaos tool catalogue. Tool availability
is updated each turn. Chaos permissions apply to every tool call.

### Model discovery under clamp

CLI-backed (`clamp = true`) sessions use cached model metadata without automatic
native API discovery or authentication. Missing metadata uses the default model
descriptor.

Explicit `refresh_models` or `models --refresh` performs discovery for the
requested provider/account and reports credential or discovery errors. A CLI
login alone does not supply native API credentials. Custom authoritative
catalogs remain unchanged.

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

- **Responses API** — `/v1/responses`.
- **Chat Completions** — `/v1/chat/completions`.
- **Anthropic Messages** — `/v1/messages` on `api.anthropic.com`.
- **LSD** — LSD's native `/inference` endpoint. Select `wire_api = "lsd"`.

For HTTP providers, `wire_api = "auto"` tries Responses first and falls back
to Chat Completions on 404/405/501. The winning format is cached for the
session. Enabled WebSocket v2 selects Responses directly.

## EXAMPLES

### OpenAI ChatGPT context window

For GPT-5.6 Sol, Chaos uses the context window advertised by the provider
catalog by default:

```toml
chatgpt_context_window = "catalog"
```

The optional `observed-400k` preset applies to `gpt-5.6-sol` on the built-in
OpenAI ChatGPT OAuth route:

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
models or providers. The provider's enforced context limit may differ from
the configured preset.

Explicit numeric settings are also available:

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

To enable bounded agent control over compaction timing:

```toml
agent_compaction_control = "bounded"
```

The default is `"disabled"`. In bounded mode, Chaos exposes a
`compaction_control` tool. The agent may request immediate compaction at the
next safe turn-loop boundary, or defer the current pressure window once after
receiving its reflex. A deferral cannot stack or change its own limits: Chaos
keeps an absolute ceiling equal to the smaller of 90% of the raw model window
and the effective input window minus a 20,000-token compaction reserve.
For the `observed-400k` preset, that ceiling is 360,000 tokens, so deferral
extends the normal 350,000-token threshold by only 10,000 tokens. Models whose
catalog window is reduced to an 80% effective input window can have a larger
usable band.
Doing nothing retains normal automatic compaction. Accepted decisions are
persisted for resume continuity; forks inherit transcript history but not a
pending decision made by the source agent.

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

# Optional idle identity marker is TUI presentation only.
[tui]
terminal_title_icon = "✦"
```

The optional idle icon prefixes both named sessions and the `new session`
fallback. It must be one grapheme cluster and no more than four terminal cells;
an empty string disables it.

Agent mode exposes `set_session_title` to the current root agent. The kernel
requests title reviews at session start, resume, compaction, reconnection, and
periodically during longer work. Keep an existing title when it still describes
the session's primary work.

The title is the process name used by `/rename` and `chaos resume`. An explicit
`/rename` prevents the agent from replacing that name. Agent-generated names
must be distinct from other unarchived sessions.

Terminal-title updates are cleared when the TUI exits. Terminal configuration
may override their display.

### xAI (Grok)

Bundled — no config needed. Just export the key:

```bash
export XAI_API_KEY=xai-...
chaos --provider xai --model grok-4
```

The exact `api.x.ai` host exposes xAI's native `web_search` and
`x_search` server-side tools. Override the provider through
`[model_providers.xai]` settings.

With xAI subscription authentication, Chaos uses the Grok subscription proxy.
The dedicated xAI adapter owns its subscription headers and model capabilities;
it shares the Responses transport with OpenAI-compatible providers. Chaos selects
it for the `xai` provider or either official xAI API/subscription endpoint.
Its model listing omits image capabilities, so Chaos enables image input for
Grok 4 chat models on that exact proxy host, including images returned by tools.
Coding models remain text-only unless discovery explicitly advertises vision.
Other endpoints retain their advertised capabilities.

After upgrading an existing subscription session, refresh its model catalog
with the session's `refresh_models` tool (`provider: "xai"`) before the next turn.
Alternatively, run the updated `chaos --provider xai models --refresh` against
the same Chaos home and runtime database, then restart the session. A restart
alone can reuse cached text-only capabilities.

Provider HTTP 402 (Payment Required) responses, including Grok Build balance
exhaustion, are reported as a non-retryable quota error (`UsageLimitExceeded`
in protocol events). Replenish the balance or switch providers before retrying.

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

The Anthropic provider uses the Messages API at `api.anthropic.com`.
Five-minute prompt caching is enabled by default. Set
`CHAOS_ANTHROPIC_CACHE_TTL=1h` for the extended TTL or
`CHAOS_ANTHROPIC_CACHE_TTL=off` to disable caching.

### LSD

```toml
[model_providers.lsd]
name = "LSD"
base_url = "http://localhost:8847"
wire_api = "lsd"
```

```bash
chaos --provider lsd --model my-function
```

LSD has its own inference protocol — explicit `wire_api = "lsd"` required.

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
routes with separate credentials.

Both use HTTP/SSE Responses requests, including function tools and native
`web_search`. Kimi's plaintext reasoning history is preserved across tool calls.
Use `low`, `high`, or `max` effort; `none`/`minimal` map to `low`, `medium` to
`high`, and `xhigh`/`ultra` to `max`.

The pay-per-token Responses endpoint currently supports `kimi-k3`; other
models advertised by `/models` may require a different wire API. Kimi Code
model access depends on your subscription. Refresh the catalog with:

```bash
chaos --provider moonshotai models --refresh
chaos --provider moonshotai-coding models --refresh
```

Context windows and image/thinking capabilities are taken from the provider's
model catalog. For a regional endpoint, override the provider's settings and
use a key for that region.
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
| `wire_api` | no | `"auto"` (default), `"responses"`, `"chat_completions"`, or `"lsd"`. Requests to `api.anthropic.com` use Messages. |
| `http_headers` | no | Static headers as `{ "Header-Name" = "value" }` |
| `env_http_headers` | no | Headers from env vars as `{ "Header-Name" = "ENV_VAR" }` |
| `query_params` | no | Query string parameters as `{ "key" = "value" }` |
| `request_max_retries` | no | HTTP retry limit (default: 4, max: 100) |
| `stream_max_retries` | no | Stream reconnect limit (default: 5, max: 100) |
| `stream_idle_timeout_ms` | no | Idle timeout in ms (default: 300000) |
| `supports_websockets` | no | Use Responses WS v2 (OpenAI: true; other providers: false by default). Set false to disable. |
| `experimental_bearer_token` | no | Bearer token or credential reference |

## SELECTION RULES

FreeChaOS resolves the wire format in this order:

1. Requests to `api.anthropic.com` → Anthropic Messages API
2. If `wire_api` is set explicitly → use it (`responses`, `chat_completions`, `lsd`)
3. Otherwise → `auto`: use Responses directly when WebSocket v2 is enabled;
   with HTTP/SSE, try Responses and fall back to Chat Completions on 404/405/501.

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

- `~/.chaos/config.toml` - bootstrap settings, including global egress

## SEE ALSO

- [chaos-install.7](./chaos-install.7.md)
- [chaos-storage.7](./chaos-storage.7.md)
- [chaos-reflex.7](./chaos-reflex.7.md)
- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-halluacinate.7](./chaos-halluacinate.7.md)
