# chaos-which path resolution

`chaos-which` resolves test binaries and fixture paths for Cargo-driven runs.

Function behavior:
- `cargo_bin`: checks `CARGO_BIN_EXE_*` environment variables first (absolute
  paths; nextest exports them for the test's own package, CI exports them for
  cross-package binaries). Otherwise it uses `<target>/<profile>/<name>` beside
  the running test executable. It never builds: a missing binary fails at once
  with the `cargo build --bin <name>` command and the variable to export.
  `cargo nextest run --workspace` builds every binary the suite spawns.
- `find_resource!`: resolves fixture paths relative to `CARGO_MANIFEST_DIR`.
- `repo_root`: walks upward from `repo_root.marker` to locate the workspace
  root from within the crate.

This crate assumes standard Cargo test/build conventions and does not rely on
external build-system path resolution.
