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

### Shared build cache

Mr. Boxington (`mbx`) is an optional Cargo frontend for local builds and the
compiler cache used by GitHub test CI. It shares cached compilations across
checkouts without sharing Cargo's target-directory lock. It is build tooling,
not a FreeChaOS runtime dependency.

The repository pins mbx 1.8.3 in `mise.toml`. On Linux or Apple Silicon macOS:

```bash
mise install github:jdx/mr-boxington
mise exec -- just cargo=mbx build
mise exec -- just cargo=mbx qa
```

Without mise, install `cargo install mbx --version 1.8.3 --locked`, then use
`just cargo=mbx build` or `just cargo=mbx qa`. Direct commands such as
`mbx check -p chaos-pwd` and `mbx nextest run -p chaos-pwd` also work.
The QA recipes still apply the usual temporary-directory and environment
isolation.

Plain `just` recipes continue to use Cargo, so mbx is not required on FreeBSD
or other hosts without a prebuilt mbx release. Formatting and dependency
maintenance also continue to use plain Cargo. This integration does not run
`mbx setup`, edit global Cargo/mise settings, or require mise's newer automatic
Cargo wrappers. To compare against an uncached invocation when a global wrapper
is already installed, use `MBX_DISABLE=1 just cargo=cargo check`.

The shared policy in `.mbx.toml` keeps build-script execution caching disabled:
`bin/chaos/build.rs` reads the clock, and native dependencies probe the host.
C/C++ caching is also disabled until it has been qualified against the SQLite
link failure previously seen with sccache. Rust compilation caching remains
enabled. Do not set `RUSTC_WRAPPER=sccache` alongside mbx: mbx defers to an
existing wrapper instead of caching those compilations.

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

### CI cache scope

`.github/workflows/rust-ci.yml` installs the SHA-pinned mbx action after Rust,
then runs both the runtime-binary build and nextest through mbx. Its GitHub
backend transports the pruned Cargo `target` tree and registry, replacing the
previous cargo-home and disabled sccache setup. Managed target views and native
link-object caching are disabled by the action in this mode, so runtime-binary
paths and Cargo timing artifacts stay under the ordinary `target/` tree.

Only successful pushes to the default branch publish a cache. Pull requests,
including forks, and manual workflow runs are restore-only. The key separates
mbx version, workspace cache policy, target triple, OS/architecture, and Rust
compiler identity. Keep the mbx version in `mise.toml`, the action input, and
the cache generation aligned when upgrading.

Release jobs, including the Gitea/FreeBSD path, are deliberately unchanged.
Do not add this shared mbx cache to production release jobs: upstream recommends
excluding remote compiler caches from published artifacts' build path. CI
cache-hit reports and timings should be measured before claiming a speedup.

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
- `./mise.toml` - pinned development tool versions, including optional mbx
- `./.mbx.toml` - shared local/CI compiler-cache policy

## SEE ALSO

- [chaos-providers.7](./chaos-providers.7.md)
- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-halluacinate.7](./chaos-halluacinate.7.md)
- [chaos-httpd.8](./chaos-httpd.8.md)
