# chaos-providers(7)

## NAME

chaos-providers - configure and select LLM providers for FreeChaOS

## DESCRIPTION

FreeChaOS is provider-agnostic. The kernel speaks the Chaos-ABI; adapters
translate that to whatever wire format a given provider expects. New
providers are added through `~/.chaos/config.toml` - no code changes, no
rebuilds.

For the condensed support matrix (bundled IDs, wire formats, OS/CI coverage, clamp backends), see [chaos-support(7)](./chaos-support.7.md).

## BUILT-IN PROVIDERS

FreeChaOS ships with these providers preconfigured:

| Provider | Wire format | Notes |
|----------|-------------|-------|
| `openai` | Responses API | Default, hardcoded |
| `anthropic` | Anthropic Messages | Hardcoded; URL-detected |
| `xai` | Responses API | Bundled; native `web_search` / `x_search` tools |
| `zai` | Chat Completions | Bundled; Z.ai pay-per-token (GLM-5 / GLM-5.1) |
| `zai-coding` | Chat Completions | Bundled; Z.ai GLM Coding Plan subscription |
| `charm` | Chat Completions | Bundled via `thirdparty.toml` |

Any other provider — DeepSeek, Groq, Ollama, MiniMax, Kimi, TensorZero,
self-hosted gateways — is a config entry away.

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
```

Agent mode exposes `set_session_title` to the current root agent and gives it
one private, persisted reminder on the first user turn of an unnamed session.
The title remains the existing process name used by `/rename` and
`chaos resume`; the separate database `title` field remains a first-message
preview and is never used as the terminal fallback. An explicit `/rename`
always marks the name as user-authored, preventing the agent from replacing it.
Agent-generated names must also be distinct from other unarchived sessions.

Chaos emits the portable OSC terminal-title sequence and clears it when the TUI
exits. Terminal configuration can still override or transform the displayed
tab title; for example, a custom WezTerm `format-tab-title` callback may choose
not to show the pane title.

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
- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-halluacinate.7](./chaos-halluacinate.7.md)
