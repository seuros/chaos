# FreeChaOS vs Claude Code vs OpenAI Codex CLI

|  | **FreeChaOS** | **Claude Code** | **OpenAI Codex CLI** |
|--|-----------|----------------|---------------------|
| **License** | Apache-2.0 | Proprietary | Apache-2.0 |
| **Provider lock-in** | None — Chaos-ABI supports any provider | Anthropic only | OpenAI only |
| **Models** | Any (OpenAI, Anthropic, Ollama, local) | Claude only | OpenAI only |
| **Architecture** | Provider-agnostic ABI, adapters are peers | Monolithic | Hardcoded to Responses API |
| **MCP support** | Full (client + server, `.mcp.json`) | Partial (no elicitation, limited resources) | Stubs (announced, mostly unimplemented) |
| **Sandbox** | Landlock + seccomp (Linux), seatbelt (macOS) | Container-based | Landlock + seatbelt |
| **Platforms** | Linux, macOS, FreeBSD | Linux, macOS, Windows | Linux, macOS, Windows |
| **External contributors** | Welcome | Not accepted | Not accepted |
| **Binary** | Single `chaos` binary | Single `claude` binary | Multiple binaries |
| **Runtime** | Native Rust | Node.js | Native Rust |
| **Phone home** | Never | Telemetry (opt-out) | Update checker + telemetry |
| **Age verification** | Everyone is 47 | None | None |
| **Extended thinking** | Native per-provider (ABI maps effort levels) | Native | Via Responses API only |
| **Prompt caching** | Per-provider (native where supported) | Native | OpenAI only |
| **Tool streaming** | Per-provider | Native | Via Responses API only |
| **Config format** | runtime DB + `.mcp.json` + `config.toml` | `settings.json` + `.mcp.json` | `config.toml` |
| **Session resume** | `chaos resume` | `claude --continue` | `codex resume` |
| **Non-interactive** | `chaos exec` | `claude -p` | Separate `codex-exec` binary |
| **Code review** | `chaos exec review` | Not built-in | `codex review` |

## Why FreeChaOS?

FreeChaOS is a fork of OpenAI's Codex CLI that broke free from provider lock-in. The core speaks a neutral ABI — the Chaos-ABI — so adding a new model provider means writing one adapter, not rewiring the entire codebase.

It ships as a single native binary with no update checker, no telemetry, no phone home. It runs on Linux, macOS, and FreeBSD.

Claude Code is excellent but proprietary and Anthropic-only. OpenAI Codex CLI is open source but hardcoded to OpenAI's wire format. FreeChaOS takes the best of both: free software, native performance, any provider.
