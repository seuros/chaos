# Adding LLM Providers

Chaos ships with OpenAI built-in. Every other provider is added through
`~/.codex/config.toml`. No code changes, no rebuilds.

---

## How it works

Providers speak one of two wire formats and Chaos detects which automatically:

- **OpenAI Responses API** — the default. Most providers clone this.
- **Anthropic Messages API** — auto-detected when the base URL contains `anthropic`.

You add a `[model_providers.<id>]` block, set your API key in the environment,
and go.

---

## Examples

### Anthropic (Claude)

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

The URL contains `anthropic`, so Chaos uses the Messages API automatically.

### ZhipuAI (GLM)

```toml
[model_providers.glm]
name = "ZhipuAI GLM"
base_url = "https://open.bigmodel.cn/api/paas/v4"
env_key = "GLM_API_KEY"
```

```bash
export GLM_API_KEY=your-key
chaos --provider glm --model glm-4-plus
```

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
| `wire_api` | no | Always `"responses"` (default). Anthropic is auto-detected from URL. |
| `http_headers` | no | Static headers as `{ "Header-Name" = "value" }` |
| `env_http_headers` | no | Headers from env vars as `{ "Header-Name" = "ENV_VAR" }` |
| `query_params` | no | Query string parameters as `{ "key" = "value" }` |
| `request_max_retries` | no | HTTP retry limit (default: 4, max: 100) |
| `stream_max_retries` | no | Stream reconnect limit (default: 5, max: 100) |
| `stream_idle_timeout_ms` | no | Idle timeout in ms (default: 300000) |
| `supports_websockets` | no | Enable WebSocket transport (default: false) |
| `experimental_bearer_token` | no | Hardcoded bearer token (discouraged — use `env_key`) |

---

## Wire format detection

Chaos picks the wire format automatically:

1. If the `base_url` contains `anthropic` → Anthropic Messages API
2. Otherwise → OpenAI Responses API

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

**Rate limiting** — Chaos retries 429s automatically. Increase
`request_max_retries` if needed.
