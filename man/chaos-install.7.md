# chaos-install(7)

## NAME

chaos-install - build, install, and debug FreeChaOS from source

## DESCRIPTION

This page describes the supported host requirements, local build commands,
installation path, and logging controls for FreeChaOS.

In an installed system, this page would typically be installed as
`share/man/man7/chaos-install.7`.

## REQUIREMENTS

| Requirement                 | Details                                                         |
| --------------------------- | --------------------------------------------------------------- |
| Operating systems           | Linux 6.10+, macOS 15+, FreeBSD 15+                            |
| Git (optional, recommended) | 2.53+                                                           |
| RAM                         | ~80 MB (MCP mode), ~128 MB (TUI mode) — MCP servers not included |

## BUILD AND INSTALL

### Install from source

Ask `chaos` to set up your environment. Then:

```bash
just install
```

That drops the `chaos` binary into `~/.cargo/bin` (release profile,
`-C target-cpu=native`). Add `~/.cargo/bin` to your `PATH` if it
isn't already.

If something is missing during the build, ask again.

### Run without installing

```bash
# Run chaos from source (debug profile).
just chaos

# Run chaos from source (release profile + target-cpu=native).
just bigbang

# Build the debug binary without running it.
just build
```

## LOGGING AND TRACING

### `--debug` flag

Pass `-d` / `--debug` to enable debug logging. Works globally across all subcommands:

```bash
chaos --debug
chaos --debug exec "say hi"
chaos exec --debug "say hi"
```

Logs are written to `~/.chaos/debug.log`.

### `RUST_LOG`

FreeChaOS also honors the `RUST_LOG` environment variable for fine-grained control.

The TUI defaults to `RUST_LOG=chaos_kern=info,chaos_console=info,mcp_guest=info` and writes logs to `~/.chaos/log/chaos-console.log`. Override with `-c log_dir=...`.

```bash
tail -F ~/.chaos/log/chaos-console.log
```

Press `ctrl+o` inside the TUI to open the log viewer as a full-screen overlay. Navigate with arrow keys / PageUp / PageDown, dismiss with `q` or `Esc`.

The non-interactive mode (`chaos exec`) defaults to `RUST_LOG=error`, printed inline.

See the Rust docs on [`RUST_LOG`](https://docs.rs/env_logger/latest/env_logger/#enabling-logging) for configuration options.

## FILES

- `~/.cargo/bin/chaos` - installed binary path used by `just install`
- `~/.chaos/debug.log` - debug log enabled by `--debug`
- `~/.chaos/log/chaos-console.log` - default TUI log file
- `./justfile` - source-tree entry point for build and run shortcuts

## SEE ALSO

- [chaos-providers.7](./chaos-providers.7.md)
- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-halluacinate.7](./chaos-halluacinate.7.md)
- [chaos-httpd.8](./chaos-httpd.8.md)
