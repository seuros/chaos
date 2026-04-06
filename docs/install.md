## Installing & building

### System requirements

| Requirement                 | Details                                                         |
| --------------------------- | --------------------------------------------------------------- |
| Operating systems           | Linux 6.10+, macOS 15+, FreeBSD 15+                            |
| Git (optional, recommended) | 2.53+                                                           |
| RAM                         | ~80 MB (MCP mode), ~128 MB (TUI mode) — MCP servers not included |

### Build from source

Ask `chaos` to set up your environment. Then:

```bash
just build
```

That's it. If something is missing, ask again.

```bash
# Run chaos.
just chaos

# Build only.
just build
```

## Tracing / verbose logging

### `--debug` flag

Pass `-d` / `--debug` to enable debug logging. Works globally across all subcommands:

```bash
chaos --debug
chaos --debug exec "say hi"
chaos exec --debug "say hi"
```

Logs are written to `~/.chaos/debug.log`.

### `RUST_LOG`

Chaos also honors the `RUST_LOG` environment variable for fine-grained control.

The TUI defaults to `RUST_LOG=chaos_kern=info,chaos_console=info,mcp_guest=info` and writes logs to `~/.chaos/log/chaos-console.log`. Override with `-c log_dir=...`.

```bash
tail -F ~/.chaos/log/chaos-console.log
```

Press `ctrl+o` inside the TUI to open the log viewer as a full-screen overlay. Navigate with arrow keys / PageUp / PageDown, dismiss with `q` or `Esc`.

The non-interactive mode (`chaos exec`) defaults to `RUST_LOG=error`, printed inline.

See the Rust docs on [`RUST_LOG`](https://docs.rs/env_logger/latest/env_logger/#enabling-logging) for configuration options.
