# chaos-support(7)

## NAME

chaos-support - FreeChaOS support matrix (providers, platforms, storage, clamp)

## DESCRIPTION

This page is the operator-facing **support matrix** for FreeChaOS. It is derived
from the tree as of the sources listed under **SOURCE OF TRUTH**, not from
marketing copy. When code and this page disagree, trust the code and file a
doc fix.

Support levels used below:

| Level | Meaning |
|-------|---------|
| **Supported** | Implemented and intended for normal use |
| **CI-tested** | Exercised by PR/CI test jobs |
| **Release-built** | Built on the release pipeline; not necessarily tested |
| **Experimental** | Implemented, fail-closed where possible, may change |
| **Config-only** | Works by configuring a wire format / URL; not a first-party product surface |
| **Not supported** | Explicitly out of scope or not implemented |

## PROVIDERS (BUNDLED)

Built-ins come from two places:

1. Hardcoded constructors in `sys/kern/kern` (`openai`, `anthropic`)
2. Bundled `lib/libnet/services/thirdparty.toml` (`xai`, `zai`, `zai-coding`, `charm`)

User entries under `[model_providers.<id>]` in `~/.chaos/config.toml` override
or extend these.

| ID | Display name | Default wire | Auth env / methods | Notes | Level |
|----|--------------|--------------|--------------------|-------|-------|
| `openai` | OpenAI | `responses` | ChatGPT account + API key (`requires_openai_auth`) | Default provider path; WebSocket support enabled in provider info | Supported |
| `anthropic` | Anthropic | `auto` (URL forces Messages) | `ANTHROPIC_API_KEY` | Base URL contains `anthropic` → Messages adapter | Supported |
| `xai` | xAI | `responses` | `XAI_API_KEY`; also `xai_account` | URLs containing `x.ai` inject native `web_search` / `x_search` | Supported |
| `zai` | Z.ai | `chat_completions` | `ZAI_API_KEY` | Pay-per-token GLM endpoint | Supported |
| `zai-coding` | Z.ai Coding Plan | `chat_completions` | `ZAI_API_KEY` | Subscription coding endpoint | Supported |
| `charm` | Charm Hyper | `chat_completions` | `CHARM_API_KEY` | Bundled third-party gateway | Supported |

### Config-only examples (not bundled)

Documented in `chaos-providers(7)`; same adapters, operator-supplied config:

| Example ID | Typical wire / routing | Env key | Level |
|------------|------------------------|---------|-------|
| `ollama` | `auto` / chat completions on local OpenAI-compatible server | none | Config-only |
| `deepseek` | OpenAI-compatible | `DEEPSEEK_API_KEY` | Config-only |
| `groq` | OpenAI-compatible | `GROQ_API_KEY` | Config-only |
| `minimax` / `kimi` | Anthropic-compatible if `base_url` contains `anthropic` | provider-specific | Config-only |
| `tensorzero` | explicit `wire_api = "tensorzero"` | optional | Config-only |
| Azure OpenAI-compatible | `responses` with Azure URL detection helpers | provider-specific | Config-only |

## WIRE FORMATS

Config enum `WireApi` (`model_provider_info`):

| `wire_api` value | HTTP surface | Parrot adapter key | Level |
|------------------|--------------|--------------------|-------|
| `auto` (default) | try Responses, fall back to Chat Completions on 404/405/501 | resolved at runtime | Supported |
| `responses` | `/v1/responses` | `responses` → `OpenAiAdapter` | Supported |
| `chat_completions` | `/v1/chat/completions` | `chat_completions` → `ChatCompletionsAdapter` | Supported |
| `tensorzero` | TensorZero `/inference` | `tensorzero` → `LsdAdapter` | Supported |

Anthropic is **not** a `wire_api` variant. Selection rules:

1. If `base_url` contains `anthropic` → Anthropic Messages (`anthropic_messages` adapter), overriding `wire_api`
2. Else if `wire_api` is set → use it
3. Else → `auto`

`chaos_parrot::adapter_for_wire` currently maps:

- `anthropic_messages`
- `responses`
- `chat_completions`
- `tensorzero`

Any other string returns `None`.

## CLAMP TRANSPORTS

When `clamp = true`, Chaos uses a first-party CLI as transport instead of a
direct provider API. Backend enum: `ClampBackend`.

| Backend | Config | External binary | Level |
|---------|--------|-----------------|-------|
| Claude Code | `clamp_backend = "claude-code"` (default) | `claude` on `PATH` | Supported |
| Antigravity | `clamp_backend = "antigravity"` | `agy` (`CHAOS_AGY_PATH` / `CHAOS_AGY_HOME`) | Experimental |

Both paths are designed to fail closed (no silent fallback to metered API
billing on auth/CLI failure). See README clamping section and `chaos-clamp`
module docs.

## PLATFORMS AND SANDBOXES

| Platform | Sandbox crate | Mechanism | Kernel / notes | CI tests | Release build | Level |
|----------|---------------|-----------|----------------|----------|---------------|-------|
| Linux x86_64 | `alcatraz-linux` | landlock + seccomp + `no_new_privs` | Requires Linux **≥ 6.10** (hard refuse on older) | Yes (`ubuntu-24.04`) | Yes | Supported + CI-tested |
| Linux aarch64 | `alcatraz-linux` | same | same | Yes (`ubuntu-24.04-arm`) | Yes | Supported + CI-tested |
| macOS aarch64 | `alcatraz-macos` | seatbelt profiles | Apple sandbox | Yes (`macos-26`) | Yes | Supported + CI-tested |
| FreeBSD x86_64 | `alcatraz-freebsd` | capsicum | Implemented | **No** PR test job | Yes (self-hosted `blackship`) | Supported; release-built, not CI-tested |
| DragonFly BSD x86_64 | (build target) | no dedicated `sys/arch` crate in-tree | Release matrix only | No | Yes (self-hosted) | Release-built only |
| OpenBSD | — | README mentions pledge/unveil | **No** `sys/arch/openbsd` crate | No | No | Not supported (aspirational docs only) |
| Windows | — | — | Explicitly unsupported | No | No | Not supported |

Source of platform claims: `sys/arch/*`, `.github/workflows/rust-ci.yml`,
`.github/workflows/release.yml`, README hardware section.

## STORAGE AND RECALL

| Backend | How selected | Level | Notes |
|---------|--------------|-------|-------|
| SQLite | default (`chaos.sqlite` under chaos home) | Supported | Default single-node store |
| PostgreSQL | `storage_url` / `CHAOS_STORAGE_URL` | Supported | Shared multi-node history; schema tested vs PG 18, needs ≥ PG 10 features |
| Semantic recall (`chaos-recall`) | same PG mount + **pgvector** | Supported (PG only) | SQLite mount returns backend error for recall |

See `chaos-storage(7)`.

## MCP / DRIVERS

| Surface | Level | Notes |
|---------|-------|-------|
| MCP client (`.mcp.json`, managed servers) | Supported | Kernel + `mcpd` runtime |
| In-tree tools / arsenal | Supported | Local FS/shell/etc. tool surface |
| External drivers (`drivers/dictator`, `drivers/helmsman`) | Separate repos/submodules | Excluded from main workspace members; own release cycles |

## WHAT IS NOT CLAIMED

- **Every** OpenAI-compatible host is not individually certified. Compatibility
  is wire-format level (`responses` / `chat_completions` / Anthropic URL /
  TensorZero).
- FreeBSD is a first-class sandbox implementation but **not** on the PR test
  matrix; treat FreeBSD regressions as higher risk until CI grows a FreeBSD
  test runner.
- OpenBSD pledge/unveil text in the README is **not** backed by an in-tree
  arch crate today.
- Antigravity clamp is experimental and depends on an official `agy` install
  plus a dedicated credential home.

## SOURCE OF TRUTH

| Topic | Primary sources |
|-------|-----------------|
| Bundled third-party providers | `lib/libnet/services/thirdparty.toml` |
| Built-in OpenAI/Anthropic + `WireApi` | `sys/kern/kern/src/model_provider_info.rs` |
| Wire → adapter map | `sys/kern/providers/parrot/src/lib.rs` (`adapter_for_wire`) |
| Clamp backends | `sys/kern/kern/src/config.rs` (`ClampBackend`), `sys/modules/clamp/` |
| Sandboxes | `sys/arch/linux`, `sys/arch/macos`, `sys/arch/freebsd`, `sys/arch/base` |
| PR CI targets | `.github/workflows/rust-ci.yml` |
| Release targets | `.github/workflows/release.yml` |
| Storage | `sys/vfs`, `man/chaos-storage.7.md` |
| Provider how-to | `man/chaos-providers.7.md` |

## SEE ALSO

`chaos-providers(7)`, `chaos-storage(7)`, `chaos-mcp(7)`, `chaos-install(7)`
