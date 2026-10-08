# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [VibeSemVer](SEMVER.md): `47.MAJOR.MINOR.TIMESTAMP`. A MAJOR
entry means read this file before upgrading. A MINOR entry means you probably
should. There is no patch level; the build timestamp is the patch.

## [Unreleased]

### Added
- `chaos update` installs the latest GitHub release after confirmation,
  verifying its SHA-256 and the complete binary bundle before atomically
  replacing it. Only GitHub release builds include it; Nix, cargo, and source
  installs are refused. Package builds omit it unless `self-update` is enabled.

### Fixed
- Reconnect ChatGPT subscription WebSockets before lazily activating native web
  search on a socket initialized without it, avoiding hosted-tool authorization
  failures without replaying requests or disabling search.
- Keep ChatGPT subscription web search text-only when its catalog advertises
  unsupported image search, avoiding `rustponsesapi` turn failures without
  changing public API or custom-endpoint search capabilities.
- `chaos://models` and `refresh_models` emit `input_modalities` when a catalog
  preset advertises image input, so hosted vision is visible in the model-facing
  catalog instead of only in the cache.

## [47.13.0] - 2026-10-07

Recall requires a PostgreSQL VFS mount with pgvector. The first activation needs
network permission unless the pinned model is already cached under `chaos_home`.
Changing models requires explicit reindexing.

### Added
- Native recall tools for scoped memory search, explicit storage and deletion,
  permission-checked source opening, and receipt-gated use reinforcement.
  Project and session memories are isolated; shared global search is opt-in.
- Local Model2Vec embeddings and concurrent semantic/lexical retrieval through
  Bonsai, with reciprocal-rank fusion, scoped deduplication, cancellation,
  deadlines, and degraded results when one retrieval branch fails.
- Opt-in automatic memory previews, treated as untrusted data. Reading a memory
  does not reinforce it; explicit use is bounded and protected against replay.
- Lazy, process-shared model preparation with pinned, verified artifacts.
  Downloads honor network grants and configured egress across redirects;
  verified cached models work offline. No sidecar or remote embedding service.

### Changed
- Consolidate terminal, multiplexer, SSH, and display detection.
- Headless builds no longer identify the terminal emulator.

### Fixed
- Desktop notifications inside tmux.
- Provider API keys with surrounding whitespace in the environment.
- Continue reading legacy `unified_exec` model catalog metadata and
  `unified_exec_startup` / `unified_exec_interaction` journal events after the
  exec rename. Newly serialized data retains the canonical `exec` names.

## [47.12.0] - 2026-10-05

### Added
- Inline Mermaid diagrams in assistant replies, rendered locally in the
  background. Terminal graphics support includes Ghostty/Kitty through tmux and
  inline image transport over SSH. The scrollable chat viewport keeps the
  composer pinned, while transcripts, copying, and fallback rendering preserve
  diagram source.
- Markdown bodies for plans and tasks, append-only clarifications, and explicitly
  requested background consolidation using a model chosen from the catalog.

### Fixed
- Keep long plans scrollable with the mouse wheel and paging keys while the
  "Implement this plan?" prompt stays open, without changing its selection or
  dismissing it.

## [47.11.0] - 2026-10-04

### Added
- Database-backed workspaces, multi-project plans, and nested tasks with immutable
  short references, independent ordering, cancellation, revision checks,
  idempotent mutations, and append-only history. PostgreSQL supports shared
  multi-machine plans and a separate dependency DAG; SQLite supports local nesting.
- Operator-facing `chaos workspace` and `chaos plan` commands, installation-local
  checkout bindings, and explicit session attachments restored from the database
  on resume.
- NixOS support: a flake (`nix profile install github:seuros/chaos`) building
  the release binary set against the pinned `rust-toolchain.toml` toolchain,
  plus a `nix develop` shell with the native build prerequisites.
  `install.sh` now detects NixOS and points at the flake instead of
  installing glibc-linked binaries that cannot run there
  (`CHAOS_ALLOW_NIXOS_BINARY=1` overrides for `programs.nix-ld` users).

### Fixed
- Compact model-facing JSON from `chaos://machine`, Skipper tools, Helmsman skill
  resources, and Dictator tools instead of spending model context on indentation.
- Wire IPC `ListModels` requests to correlated, local-only catalog responses
  without starting a model turn or persisting catalog data in session history.
- Honor custom CA bundles and client certificate/private key pairs in OTLP
  HTTP exporters for logs, traces, and metrics. Invalid TLS files or incomplete
  or mismatched mTLS identities now fail exporter initialization.

### Changed
- Expose plan authoring only in Plan mode and compact progress tools only when
  an execution-mode session has an attached plan. Render planning instructions
  and schemas from the mounted database's capabilities.
- Render committed plan snapshots with stable task references, nesting, statuses,
  and revisions in the terminal UI and JSONL output.
- Consolidate managed sessions and one-shot process execution under `exec`, retaining
  PTYs, pipes, stdin, background tracking, direct argv, hard timeouts, and cancellation.
  Shell model metadata now uses `exec`; execution-source tags are `exec_startup` and
  `exec_interaction`, without compatibility aliases. Tool names `exec_command` and
  `write_stdin` are unchanged.
- Replace FFF-backed file-search sessions with a single root-scoped walker and
  local fuzzy ranking. Content grep uses byte regexes and ignore-aware traversal
  without a Git backend or external binary.

### Removed
- The transcript-only `update_plan` tool and legacy todo-list output. Finishing
  a model turn no longer fabricates plan completion.
- `fff-search` and its transitive `git2`/vendored-libgit2 dependency.

## [47.10.2] - 2026-10-03

### Added
- Automatic terminal-title attention markers alternating between `◻` and `❏`
  for pending approvals, permission requests, user-input questions, and MCP
  elicitation, including queued and background-agent requests. These markers
  are built-in, with no override. Attention takes precedence over working and
  idle markers, restoring the normal title when requests are answered or
  cancelled.
- MCP Apps `ui://` resource discovery and reads with typed tool UI metadata,
  HTML decoding, and validated sandbox metadata. Terminal clients continue not
  to advertise Apps rendering support.

### Changed
- Animate working terminal titles through the built-in `◰`, `◱`, `◲`, and `◳`
  markers, with no activity-animation toggle.
- Update `mcp-host` and its macros to 0.6.0 and `mcp-guest` to 0.11.0.

### Removed
- The `tui.terminal_title_working_icon` override; working and attention
  activity markers are built-in rather than configurable replacements.
- Obsolete steer-mode test shim, redundant shell runtime backend selection, and
  the unused `ConversationPathResponseEvent` type.

## [47.10.1] - 2026-10-02

### Added
- Payload-free direct-child lifecycle updates in `chaos exec --json`, independent
  of collaboration tool results. Failed initialization does not create a pending
  child entry.

### Changed
- Replace hand-rolled kernel, UI, and HTTP transport lifecycles with the existing
  `state-machines` crate while preserving persisted state formats and recovery
  behavior.
- Update the Skipper driver to 0.4.0 with PR watching, discussion/list resources,
  and workspace metadata. Building the driver now requires Rust 1.99.
- Replace the Git fork dependencies for `ratatui-hypertile` and
  `ratatui-hypertile-extras` with their upstream crates.io 0.4.2 releases,
  adapting to the palette API while preserving pane filtering and selection handling.

### Fixed
- Resolve patch scenario fixtures relative to their own crate for reliable QA
  runs.
- Keep request-only runtime guidance and machine warnings out of native resume
  checkpoints, preserving Claude and Antigravity tool history across turns while
  still delivering current guidance on every request.
- Update prompt-caching tests to verify request-local guidance separately from
  the canonical conversation prefix.

## [47.10.0] - 2026-10-01

Upgrading: integrations must use `user_turn` with `vfs_policy` and
`socket_policy`. Legacy session records using `sandbox_policy` or compaction
records without `replacement_history` are no longer supported.

### Changed
- Simplified built-in model guidance and limited terminal formatting instructions
  to interactive TUI sessions.
- Mouse-wheel and keyboard paging now scroll output directly in the chat pane,
  keeping the composer and other panes visible instead of opening the transcript.
  Incoming output preserves the reading position; `End` or `Esc` resumes following
  live output, while `Ctrl+T` explicitly opens the transcript viewer.
- Resuming a session already in use now shows a concise message with retry
  guidance instead of nested journal errors; diagnostic details remain in logs.

### Removed
- Legacy `user_input` submissions; callers must send `user_turn` with turn context.
- The single `sandbox_policy` field in `user_turn`; turn submissions now carry
  `vfs_policy` and `socket_policy` directly, preserving fine-grained restrictions.
- Sandbox aliases and the single `sandbox_policy` field in stored turn contexts
  and session-configured events. Both `vfs_policy` and `socket_policy` are required.
- Compaction records without `replacement_history`. Old records are rejected;
  no replay fallback or automatic migration remains.
- Deprecated raw configuration loading, automatic personality migration, PTY
  type aliases, and the unused ConPTY support shim.

## [47.9.0] - 2026-09-30

Upgrading: already-migrated credential vaults continue to work unchanged.
If credentials still use the pre-47.8 per-item Keychain store, stop ChaOS and
run `chaos config migrate-secrets` with **47.8.x before installing 47.9.0**, or
re-enter credentials in the new version. See the
[upgrade guide](README.md#upgrading-to-4790).

### Added
- Explicit `chaos hooks --yes` operator provisioning without a terminal prompt,
  including disabled legacy import followed by deliberate enabling.
- Opt-in `hook_approval_policy = "automatic"` for unattended native hook-tool
  management. Human elicitation remains the default; revision checks,
  installation-local grants, project trust, and execution sandboxing are unchanged.

### Fixed
- Agent-role application preserves the parent's hook approval policy, preventing
  trusted project roles from enabling automatic hook authorization.
- Bound the turn task's inline future size when loading database hooks, avoiding
  worker-thread stack overflows. Keep integration-test homes and working
  directories alive for lifecycle hook resolution.

### Removed
- `chaos config migrate-secrets` and the legacy settings, MCP, and provider
  Keychain import paths, including import-only vault APIs. No alias or runtime
  fallback remains. The separate `chaos config migrate` settings command is
  unchanged.

## [47.8.0] - 2026-09-29

Upgrading: stop older ChaOS processes and run `chaos config migrate-secrets`
with the new binary before restarting. The one-time import may prompt for each
legacy Keychain item. It preserves credential references and MCP approval
identities; normal operation never falls back to those legacy items. Do not run
older binaries against the upgraded vault. See
[credential vault migration](docs/database-configuration.md#credential-vault-and-macos-prompts).

### Added
- Explicit, retryable `chaos config migrate-secrets` command. Source Keychain
  items are retained for recovery; retries never overwrite live credentials or
  resurrect deleted ones. This command will be removed in **47.9.0**; migrate
  with **47.8.x** before upgrading further. See the
  [upgrade guide](README.md#upgrading-to-4790).

### Changed
- Settings, MCP credentials, named secrets, and provider auth in `keyring`/`auto`
  mode share one encrypted vault per ChaOS home, with one OS-held unlock key
  cached per process.
- Vault schema v2 uses process-shared snapshots, cross-process write locking,
  atomic owner-only writes, and deletion records that preserve logout across
  migration retries. Back up both the encrypted vault and its unlock key.

### Removed
- Plaintext fallback for provider `auto` auth. If it previously used
  `auth.json`, explicitly select `file` mode or reconnect the account into the
  vault. The existing default `file` mode and `ephemeral` mode are unchanged.
- FreeBSD onboarding's plaintext connection-URL fallback. Use `env:VARIABLE`
  when the OS credential store is unavailable.

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

[Unreleased]: https://github.com/seuros/chaos/compare/v47.12.0...HEAD
[47.12.0]: https://github.com/seuros/chaos/compare/v47.11.0...v47.12.0
[47.10.0]: https://github.com/seuros/chaos/compare/v47.9.0...v47.10.0
[47.9.0]: https://github.com/seuros/chaos/compare/v47.8.0...v47.9.0
[47.8.0]: https://github.com/seuros/chaos/compare/v47.7.1...v47.8.0
[47.7.1]: https://github.com/seuros/chaos/compare/v47.7.0...v47.7.1
[47.7.0]: https://github.com/seuros/chaos/compare/v47.6.0...v47.7.0
[47.6.0]: https://github.com/seuros/chaos/releases/tag/v47.6.0
