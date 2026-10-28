+++
title = "chaos-support(7)"
summary = "Available providers, platforms, transports, and storage."
+++

# chaos-support(7)

## NAME

chaos-support - FreeChaOS support matrix (providers, platforms, storage, clamp)

## DESCRIPTION

This page lists available providers, platforms, transports, and storage
backends. See the linked manuals for configuration and runtime behavior.

Support levels used below:

| Level | Meaning |
|-------|---------|
| **Supported** | Implemented and intended for normal use |
| **Experimental** | Implemented, fail-closed where possible, may change |
| **Config-only** | Selected through a custom provider entry |

## PROVIDERS (BUNDLED)

Persisted `model_providers.<id>` settings, written with `chaos config set`,
override or extend the built-in providers.

| ID | Display name | Default wire | Auth env / methods | Notes | Level |
|----|--------------|--------------|--------------------|-------|-------|
| `openai` | OpenAI | `responses` | ChatGPT account + API key | Default provider; Responses WebSocket v2 | Supported |
| `anthropic` | Anthropic | Messages | `ANTHROPIC_API_KEY` | Native API at `api.anthropic.com` | Supported |
| `xai` | xAI | `responses` | `XAI_API_KEY`; also `xai_account` | URLs containing `x.ai` inject native `web_search` / `x_search` | Supported |
| `moonshotai` | Moonshot AI | `responses` | `MOONSHOT_API_KEY` | Pay-per-token API; Kimi K3; native `web_search` | Supported |
| `moonshotai-coding` | Moonshot AI Coding | `responses` | `KIMI_API_KEY` | Kimi Code subscription endpoint; native `web_search` | Supported |
| `zai` | Z.ai | `chat_completions` | `ZAI_API_KEY` | Pay-per-token GLM endpoint | Supported |
| `zai-coding` | Z.ai Coding Plan | `chat_completions` | `ZAI_API_KEY` | Subscription coding endpoint | Supported |
| `charm` | Charm Hyper | `chat_completions` | `CHARM_API_KEY` | Bundled third-party gateway | Supported |

### Config-only examples (not bundled)

Configured as described in [chaos-providers(7)](./chaos-providers.7.md):

| Example ID | Typical wire / routing | Env key | Level |
|------------|------------------------|---------|-------|
| `ollama` | `auto` / chat completions on local OpenAI-compatible server | none | Config-only |
| vLLM, llama.cpp, LocalAI, SGLang, NIM, DGX Spark | `chat_completions` on a local or LAN OpenAI-compatible server; see [chaos-local-models(7)](./chaos-local-models.7.md) | none, or server `--api-key` | Config-only |
| `deepseek` | OpenAI-compatible | `DEEPSEEK_API_KEY` | Config-only |
| `groq` | OpenAI-compatible | `GROQ_API_KEY` | Config-only |
| `lsd` | explicit `wire_api = "lsd"` | optional | Config-only |
| Azure OpenAI-compatible | `responses` over HTTP/SSE | provider-specific | Config-only |

## REFLEX BACKENDS

Set up with `/reflex`; test the configured action-risk backend with
`/reflex test` or `chaos reflex test`.
Only action risk runs automatically; other judgments are library-only.

| `kind` | Transport | Judgments | Authentication | Level |
|--------|-----------|-----------|----------------|-------|
| `jev` | Decisions API (TypeSafe or OpenRouter) | action risk, grounding, policy violation | API key required | Experimental |
| `minicheck` | OpenAI-compatible chat completions | grounding | optional API key | Experimental |
| `shieldgemma` | OpenAI-compatible chat completions | policy violation | optional API key | Experimental |

See [chaos-reflex(7)](./chaos-reflex.7.md).

## WIRE FORMATS

| `wire_api` value | Endpoint selection |
|------------------|--------------------|
| `auto` (default) | Responses directly with WebSocket v2; HTTP/SSE tries Responses, then Chat Completions on 404/405/501 |
| `responses` | `/v1/responses` |
| `chat_completions` | `/v1/chat/completions` |
| `lsd` | LSD `/inference` |

`api.anthropic.com` uses Anthropic Messages regardless of `wire_api`.

## CLAMP TRANSPORTS

Clamp uses a first-party CLI instead of a direct provider API:

| Backend | Config | External binary | Level |
|---------|--------|-----------------|-------|
| Claude Code | `clamp_backend = "claude-code"` (default) | `claude` on `PATH` | Supported |
| Antigravity | `clamp_backend = "antigravity"` | `agy` (`CHAOS_AGY_PATH` / `CHAOS_AGY_HOME`) | Experimental |

CLI and authentication failures terminate the turn with an error.
See [setup and commands](./chaos-providers.7.md#clamp-transports).

## PLATFORMS

| Platform | Requirement |
|----------|-------------|
| Linux x86_64 / aarch64 | Linux **≥ 6.10** |
| macOS aarch64 | macOS **≥ 15** |
| FreeBSD x86_64 | FreeBSD **≥ 15** |

## STORAGE AND RECALL

| Backend | How selected | Level | Notes |
|---------|--------------|-------|-------|
| SQLite | `storage_url` / `CHAOS_STORAGE_URL` / first-run setup | Supported | Single-node store |
| PostgreSQL | `storage_url` / `CHAOS_STORAGE_URL` | Supported | Shared multi-node history; PostgreSQL ≥ 10 |
| Semantic recall (`chaos-recall`) | same PG mount + **pgvector** | Supported (PG only) | SQLite mount returns backend error for recall |

See [chaos-storage(7)](./chaos-storage.7.md).

## MCP / DRIVERS

| Surface | Level | Notes |
|---------|-------|-------|
| MCP client (`.mcp.json`, managed servers) | Supported | Local stdio and remote HTTP servers |
| Session tools | Supported | Filesystem, shell, and other activated tools |
| External MCP servers | Supported | Tools and resources from configured server connections |

## SEE ALSO

- [chaos-providers(7)](./chaos-providers.7.md)
- [chaos-local-models(7)](./chaos-local-models.7.md)
- [chaos-storage(7)](./chaos-storage.7.md)
- [chaos-mcp(7)](./chaos-mcp.7.md)
- [chaos-install(7)](./chaos-install.7.md)
