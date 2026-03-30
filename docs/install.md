## Installing & building

### System requirements

| Requirement                 | Details                                                         |
| --------------------------- | --------------------------------------------------------------- |
| Operating systems           | macOS 12+, Ubuntu 20.04+/Debian 10+, FreeBSD                   |
| Git (optional, recommended) | 2.23+ for built-in PR helpers                                   |
| RAM                         | 4-GB minimum (8-GB recommended)                                 |

### Build from source

Ask `codex` or `claude` to set up your environment. Then:

```bash
just build
```

That's it. If something is missing, ask again.

```bash
# Just talk.
just talk

# Just build.
just build
```

## Tracing / verbose logging

Chaos honors the `RUST_LOG` environment variable.

The TUI defaults to `RUST_LOG=codex_core=info,codex_tui=info,codex_rmcp_client=info` and writes logs to `~/.chaos/log/codex-tui.log`. Override with `-c log_dir=...`.

```bash
tail -F ~/.chaos/log/codex-tui.log
```

The non-interactive mode (`codex exec`) defaults to `RUST_LOG=error`, printed inline.

See the Rust docs on [`RUST_LOG`](https://docs.rs/env_logger/latest/env_logger/#enabling-logging) for configuration options.
