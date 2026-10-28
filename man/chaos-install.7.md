+++
title = "chaos-install(7)"
summary = "Build, installation, and logging."
+++

# chaos-install(7)

## NAME

chaos-install - build, install, and debug FreeChaOS from source

## DESCRIPTION

Build, install, and diagnose FreeChaOS.

## REQUIREMENTS

| Requirement                 | Details                                                         |
| --------------------------- | --------------------------------------------------------------- |
| Operating systems           | Linux 6.10+, macOS 15+, FreeBSD 15+                            |
| Rust (source builds)        | 1.98+ (`rust-version`); development uses 1.99 via `mise`/`rust-toolchain.toml`. FreeBSD's ports `rust` (1.98.1) builds it |
| Git (optional, recommended) | 2.53+                                                           |
| RAM                         | ~80 MB (MCP mode), ~128 MB (TUI mode) — MCP servers not included |

## BUILD AND INSTALL

### Prebuilt releases

Use the repository's `install.sh` for stable releases. It requires `sha256sum`,
`shasum`, or FreeBSD `sha256`; bad checksums and incomplete bundles stop installation.
Checksums from the same release are not independent signatures. Binary replacement
is not transactional; check the installed bundle after an interrupted update.

### NixOS

On NixOS, install from the repository flake instead of running `install.sh`:
the prebuilt Linux binaries are glibc-linked and will not start on NixOS.

```bash
nix profile install github:seuros/chaos
```

`install.sh` detects NixOS and refuses to run unless
`CHAOS_ALLOW_NIXOS_BINARY=1` is set (only sensible with `programs.nix-ld`
enabled). `nix develop` provides the pinned Rust toolchain and the native
build prerequisites for source installs.

### Per-commit builds

Per-commit Linux x86_64 and Apple Silicon bundles use the immutable prerelease
`build-<full-commit-SHA>`. Download these explicitly rather than using the stable
installer. On Apple Silicon macOS with GitHub CLI:

```bash
sha='REPLACE_WITH_FULL_COMMIT_SHA'
asset="chaos-macos-aarch64-${sha}.tar.gz"
mkdir "chaos-${sha}" && cd "chaos-${sha}"
gh release download "build-${sha}" --repo seuros/chaos \
  --pattern "$asset" --pattern "$asset.sha256"
shasum -a 256 -c "$asset.sha256" && \
  tar -xzf "$asset" && sh ./install.sh
```

Linux assets use `chaos-linux-x86_64-<full-commit-SHA>.tar.gz` and require
Debian bookworm-compatible glibc (2.36+) and `libdbus-1-3`.
Intel Mac bundles are not available.

### Install from source

```bash
just install
```

That drops the `chaos` binary into `~/.cargo/bin` (release profile,
`-C target-cpu=native`). Add `~/.cargo/bin` to your `PATH` if it
isn't already.

### Drivers

Model-facing git and forge tools are not built into `chaos`. They come from
[skipper](https://github.com/seuros/skipper), an external MCP driver vendored
at `drivers/skipper`. Without it the model has no `git_*` tools and no CI or
pull request visibility.

Install the `skipper-mcp` binary with the prebuilt installer:

```bash
curl -fsSL https://raw.githubusercontent.com/seuros/skipper/master/scripts/install.sh | bash
```

Or from this checkout:

```bash
just install-skipper
```

Then register it once as a global stdio server:

```bash
chaos mcp add skipper -- skipper-mcp
```

Local git tools need nothing else. Forge tools appear only for repositories
whose remote points at a forge whose CLI is installed and authenticated: `gh`
for GitHub, `glab` for GitLab, `tea` for Gitea and Forgejo. Run the CLI's
login command once; skipper stores no tokens of its own.

### Run without installing

```bash
# Run chaos from source (debug profile).
just chaos

# Run chaos from source (release profile + target-cpu=native).
just bigbang

# Build the debug binary without running it.
just build
```

### Build without the TUI, ACP, or MCP daemon

The `tui`, `acp`, and `mcpd` features are enabled by default.
Use `--no-default-features` for a minimal CLI, then select what to retain:

```bash
cargo build --release --locked -p chaos-cli --bin chaos --no-default-features

# Keep only the TUI (no ACP agent or MCP daemon).
cargo build --release --locked -p chaos-cli --bin chaos --no-default-features --features tui

# Keep only the ACP agent (no TUI or MCP daemon).
cargo build --release --locked -p chaos-cli --bin chaos --no-default-features --features acp

# Keep only the MCP daemon (no TUI or ACP agent).
cargo build --release --locked -p chaos-cli --bin chaos --no-default-features --features mcpd

# HTTP daemon; configure provider credentials first and supply a bearer token.
# CHAOS_BEARER_TOKEN must be set in the service environment.
./target/release/chaos serve --bind 127.0.0.1 --port 4000
```

The minimal binary retains `serve`, `taskd`, `exec` (including
`exec resume`), review, account/configuration management, models, and completions.
Without `acp`, it omits `acp` from commands, help, and completions.
MCP client commands, external tools, and clamping remain available.
Without `mcpd`, MCP serving flags are absent and `/mcp` returns 404;
ordinary HTTP trigger/session APIs remain available.
Without `tui`, it omits the interactive prompt, `resume`/`fork` pickers, and TUI-only flags.
Running it without a subcommand returns an explanatory error, not a daemon.
Use launchd/systemd to supervise the chosen foreground service.

This changes build contents, not server permissions or protocols: HTTP remains
authenticated, and enabling `mcpd` restores stdio and opt-in HTTP MCP serving.
See [chaos-httpd.8](./chaos-httpd.8.md) and [chaos-mcp.7](./chaos-mcp.7.md).

Build only `-p chaos-cli`; a workspace-wide build still builds the console and
ACP and MCP daemon workspace members. `--all-features` enables all three again.
Features are additive: use `--no-default-features` before selecting any subset
with `--features tui,acp,mcpd`. The runtime `--headless` approval flag is separate
from these Cargo features.

### Shared build cache

Mr. Boxington (`mbx`) optionally caches compilations across checkouts.
It is not required to build or run FreeChaOS.

The repository pins mbx 1.8.3 in `mise.toml`. On Linux or Apple Silicon macOS:

```bash
mise install github:jdx/mr-boxington
mise exec -- just cargo=mbx build
mise exec -- just cargo=mbx qa
```

Without mise, install `cargo install mbx --version 1.8.3 --locked`, then use
`just cargo=mbx build` or `just cargo=mbx qa`. Direct commands such as
`mbx check -p chaos-pwd` and `mbx nextest run -p chaos-pwd` also work.

Plain `just` recipes, formatting, and dependency maintenance use Cargo.
For an uncached check, use `MBX_DISABLE=1 just cargo=cargo check`.
Do not combine mbx with `RUSTC_WRAPPER=sccache`.

Existing `target/` directories are preserved unless an interactive user accepts
mbx's migration prompt. New checkouts may get a managed `target` symlink; use
`MBX_TARGET_VIEWS=0` to keep ordinary directories. Explicit `CARGO_TARGET_DIR`
and `--target-dir` settings are respected. Concurrent builds in the same
checkout still need separate target directories to avoid Cargo's lock.
For sandboxed agents, both the cache and the target location must be writable;
`MBX_CACHE_DIR` can place the cache inside an allowed directory.

Inspect local cache activity with:

```bash
mbx doctor
mbx cache stats
mbx gc --dry-run
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

Press `ctrl+o` inside the TUI to open the log viewer as a full-screen overlay. Navigate with arrow keys / PageUp / PageDown, dismiss with `q` or `Ctrl+C`.
See [chaos-keyboard.7](./chaos-keyboard.7.md) for the full shortcut reference, or
press `?` with an empty input for in-app help.

The non-interactive mode (`chaos exec`) defaults to `RUST_LOG=error`, printed inline.

### Startup checkpoints

For slow startup, retain `chaos_snitch::startup=info` in `RUST_LOG` (enabled by
default). Compare `stage`, `elapsed_ms`, and `duration_ms` to locate delays.
A visible first frame does not mean the session or MCP servers are ready.

See the Rust docs on [`RUST_LOG`](https://docs.rs/env_logger/latest/env_logger/#enabling-logging) for configuration options.

## FILES

- `~/.cargo/bin/chaos` - installed binary path used by `just install`
- `~/.cargo/bin/skipper-mcp` - git/forge MCP driver installed by `just install-skipper`
- `~/.chaos/debug.log` - debug log enabled by `--debug`
- `~/.chaos/log/chaos-console.log` - default TUI log file
- `./justfile` - source-tree entry point for build and run shortcuts
- `./mise.toml` - pinned development tool versions, including optional mbx
- `./.mbx.toml` - shared local/CI compiler-cache policy

## SEE ALSO

- [chaos-keyboard.7](./chaos-keyboard.7.md)
- [chaos-providers.7](./chaos-providers.7.md)
- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-halluacinate.7](./chaos-halluacinate.7.md)
- [chaos-httpd.8](./chaos-httpd.8.md)
