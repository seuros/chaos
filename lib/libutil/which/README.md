# chaos-which runfiles strategy

We disable the directory-based runfiles strategy and rely on the manifest
strategy across all platforms. This avoids Windows path length issues and keeps
behavior consistent in local and remote builds on all platforms. When
`RUNFILES_MANIFEST_FILE` is present, the `chaos-which` helpers use the
`runfiles` crate to resolve runfiles via that manifest.

Function behavior:
- `cargo_bin`: reads `CARGO_BIN_EXE_*` environment variables and resolves them
  via the runfiles manifest when `RUNFILES_MANIFEST_FILE`
  is present. When not under runfiles, it only accepts absolute paths from
  `CARGO_BIN_EXE_*` and returns an error otherwise.
- `find_resource!`: used by tests to locate fixtures. It chooses the
  runfiles resolution path when `RUNFILES_MANIFEST_FILE` is set, otherwise it
  falls back to a `CARGO_MANIFEST_DIR`-relative path for Cargo runs.
