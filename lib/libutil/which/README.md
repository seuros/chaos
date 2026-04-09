# chaos-which path resolution

`chaos-which` resolves test binaries and fixture paths for Cargo-driven runs.

Function behavior:
- `cargo_bin`: checks `CARGO_BIN_EXE_*` environment variables first. These are
  expected to be absolute paths in `cargo test`. If they are missing, it falls
  back to `assert_cmd::Command::cargo_bin`.
- `find_resource!`: resolves fixture paths relative to `CARGO_MANIFEST_DIR`.
- `repo_root`: walks upward from `repo_root.marker` to locate the workspace
  root from within the crate.

This crate assumes standard Cargo test/build conventions and does not rely on
external build-system path resolution.
