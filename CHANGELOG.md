# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [VibeSemVer](SEMVER.md): `47.MAJOR.MINOR.TIMESTAMP`. A MAJOR
entry means read this file before upgrading. A MINOR entry means you probably
should. There is no patch level; the build timestamp is the patch.

## [Unreleased]

### Added
- Explicit `chaos hooks --yes` operator provisioning without a terminal prompt,
  including disabled legacy import followed by deliberate enabling.
- Opt-in `hook_approval_policy = "automatic"` for unattended native hook-tool
  management. Human elicitation remains the default; revision checks,
  installation-local grants, project trust, and execution sandboxing are unchanged.

### Fixed
- Bound the turn task's inline future size when loading database hooks, avoiding
  worker-thread stack overflows. Keep integration-test homes and working
  directories alive for lifecycle hook resolution.

## [47.7.1] - 2026-09-28

### Added
- Optional `case_sensitive` control for `grep_files`, using fff-search 0.11's
  explicit case modes while preserving smart-case matching by default.

### Changed
- Upgrade gix to 0.88 and use structured error classification to distinguish
  missing references, invalid inputs, and other Git failures.
- Refresh Cargo dependencies, including bonsai-bt 0.14, mcp-host 0.5.3, and
  usage-rs 6.12.

## [47.7.0] - 2026-09-28

### Added
- Database-backed lifecycle hooks for `session_start`, `before_turn`, and `stop`,
  with global and project scopes in SQLite/PostgreSQL. See
  [chaos-hooks(7)](man/chaos-hooks.7.md).
- Interactive `chaos hooks` management and optional atomic file import with
  hooks disabled by default.
- `chaos://hooks` resources and revision-checked `hooks_*` tools requiring human
  elicitation for every mutation, including disable and delete.
- Installation-local execution approvals, lifecycle refresh, sandbox enforcement,
  bounded output, and process-group cleanup on timeout or cancellation.

### Changed
- Built-in resource tools are available without external MCP servers.

## [47.6.0] - 2026-09-23

Upgrading: the model loses its git tools until skipper is installed and
registered. Install `skipper-mcp` (`just install-skipper` or the installer at
https://github.com/seuros/skipper), then run `chaos mcp add skipper -- skipper-mcp`.
See the Drivers section of `man/chaos-install.7.md`.

### Added
- `drivers/skipper` submodule: MCP driver for local git and GitHub, GitLab,
  Gitea, and Forgejo read access.

### Removed
- In-tree `git_*` tools and the `git` / `git-write` tool groups. Local git
  access for the model now comes from skipper.
- `git://branches` resource template.

[Unreleased]: https://github.com/seuros/chaos/compare/v47.7.1...HEAD
[47.7.1]: https://github.com/seuros/chaos/compare/v47.7.0...v47.7.1
[47.7.0]: https://github.com/seuros/chaos/compare/v47.6.0...v47.7.0
[47.6.0]: https://github.com/seuros/chaos/releases/tag/v47.6.0
