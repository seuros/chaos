# Adding LLM Providers

FreeChaOS is provider-agnostic. The kernel speaks the Chaos-ABI; adapters
translate that to whatever wire format a given provider expects. New
providers are added through `~/.chaos/config.toml` — no code changes, no
rebuilds.

---

## What's built in

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

---

## How it works

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

---

## Examples

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

---

## Config reference

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

---

## Wire format selection

FreeChaOS resolves the wire format in this order:

1. If the `base_url` contains `anthropic` → Anthropic Messages API (overrides `wire_api`)
2. If `wire_api` is set explicitly → use it (`responses`, `chat_completions`, `tensorzero`)
3. Otherwise → `auto`: try Responses, fall back to Chat Completions on 404/405/501

There is no `wire_api = "anthropic"` option. The URL is the signal.

---

## Troubleshooting

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
