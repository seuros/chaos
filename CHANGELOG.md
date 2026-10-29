# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [VibeSemVer](SEMVER.md): `47.MAJOR.MINOR.TIMESTAMP`. A MAJOR
entry means read this file before upgrading. A MINOR entry means you probably
should. There is no patch level; the build timestamp is the patch.

## [Unreleased]

### Added
- Clamp sessions learn the model's context window from Claude Code's
  `result.modelUsage` and record it when the catalog has no entry for the
  slug, so the compaction reflex, auto-compaction and `compaction_control`
  work under clamp without a hardcoded model table. Explicit
  `model_context_window` and `model_auto_compact_token_limit` still win.

## [47.14.7] - 2026-10-28

### Added
- MCP resource reads and subscriptions accept an optional `parameters` map
  with a URI template, using `mcp-host` 0.15 to percent-encode raw string values
  such as `github-actions[bot]` before sending the request.

### Changed
- Prefer major-only requirements for stable dependencies (including ACP,
  schemars, Tokio, and UUID), with exact resolved versions in `Cargo.lock`.
- Refresh workspace dependencies, including ACP 3.6, Tokio 1.54, ICU4X 2.4,
  resvg 0.49, LRU 0.19, and vt100 0.18. Caches allocate lazily; terminal tests
  use native scrollback-aware resizing. Remove unused dependencies and enable
  Rust-version-aware dependency resolution.
- Use unicode-width 0.2.3's native halfwidth sound-mark measurements, removing
  the compatibility adjustment that would now double-count their columns.
- ACP 3.6 validates initialization negotiation inside the SDK; incompatible
  replies surface as protocol errors and still close the transport.
- Keep OpenTelemetry on 0.32 and `time` on 0.3.47 to match Rama 0.4's APIs
  and exact version requirements.

### Fixed
- `/mcp` omits resource and resource-template sections for servers that do not
  advertise the MCP `resources` capability, rather than showing `(none)`.

## [47.14.6] - 2026-10-27

### Added
- `hook_output_limit_kib` config key: the per-stream cap on hook stdout/stderr,
  default 128 KiB as before. A stream over the cap fails the hook rather than
  being truncated, which silently dropped a session-start hook's whole
  `additionalContext` once a resident's identity and journals outgrew 128 KiB.
  Installs that inject large context raise the key; nothing changes for the rest.

### Fixed
- `/mcp` uses shared connection-manager startup state instead of configuration
  enablement. Failed, cancelled, and uninitialized servers are hidden; ready
  servers show `connected` and pending servers show `starting`, even when no
  tools or resources are available.

## [47.14.5] - 2026-10-24

### Fixed
- Resumed sessions no longer inherit a permanently closed background-wake
  policy from the previous session's shutdown. Operator input can re-enable
  automatic wakes, including opt-in machine-recovery wakes. Live shutdowns
  remain irreversible, unattended recovery still excludes closed sessions,
  and saved machine-recovery waits are not replayed.

## [47.14.4] - 2026-10-22

### Fixed
- `/model` and ACP share a catalog spanning authenticated provider accounts,
  with account-qualified selections and provider-isolated credentials,
  metadata, and transports.

## [47.14.3] - 2026-10-21

### Added
- Raw macOS IOPM return code/warning level and NSProcessInfo thermal state
  in `chaos://machine` under `machine.thermal.macos`, so sessions reporting
  different merged thermal states can identify the source without changing
  warning or recovery policy.
- ACP slash-command discovery and native dispatch for session controls,
  compaction, reviews, renaming and stored custom prompts. Custom prompt
  argument expansion is shared with the console; console-only UI actions
  are not advertised.
- ACP editor-backed single-file creation, with existing-parent policy and
  identity checks, confirmed absence on disk and in the editor, and native
  approval. Creation never falls back to a disk write or retries an
  unacknowledged client write.

### Fixed
- Resuming a conversation restores acknowledged tool-group activation,
  including web-search declarations required to replay native search history.
  Failed, incomplete, or malformed activation calls are not replayed.
- Provider "response protection is unavailable" errors use retryable
  service-unavailable handling instead of generic stream-disconnect errors.
- Native session renaming targets the session's mounted runtime store rather
  than the first store mounted in the process.

## [47.14.2] - 2026-10-20

### Fixed
- Unconfigured or unavailable reflex checks no longer block MCP calls or add
  approval prompts; ordinary tool approval and sandbox rules still apply.
- ACP request timeouts no longer close established connections or replay
  requests. Permission deadlines deny access; uncertain editor writes still
  invalidate their session.

## [47.14.1] - 2026-10-20

### Added
- ACP model/mode selectors and durable session list/load/resume, with public
  history replay and native policy checks before restoring a session.
- Capability-negotiated editor text reads and approved single-file text
  updates through ACP, including unsaved text and bounded client requests.
- Optional `acp-host` backend hooks for session controls, durable sessions,
  and typed editor text access.

### Fixed
- ACP cancellation keeps sessions busy until cleanup joins; uncertain prompts
  and unacknowledged editor writes invalidate the session without retries or
  falling back to disk writes.

## [47.14.0] - 2026-10-20

### Removed (MAJOR: read before upgrading)
- TOML is no longer a configuration source. `/etc/chaos/config.toml` and
  project `.chaos/config.toml` files are not read. `$CHAOS_HOME/config.toml`
  is a strict two-key bootstrap file (`storage_url`, `egress_url`); any other
  key stops startup with the `chaos config set` command that replaces it.
- `chaos config migrate` and the TOML-to-database migration path. Settings
  already in the database are unaffected.
- The `sqlite_home` bootstrap key and the `CHAOS_SQLITE_HOME` environment
  variable. Name the SQLite file in the storage URL instead
  (`sqlite:///path/chaos.sqlite`); `CHAOS_STORAGE_URL` is the only storage
  environment variable.
- IPC: `ConfigLayerSource::{System, User, Bootstrap, Project}`, the `filePath`
  fields on config write requests/responses, and the `configLayerReadonly`
  error code.

### Changed
- Storage is never chosen silently. Every command requires `storage_url` or
  `CHAOS_STORAGE_URL`; the console runs first-run setup when neither is set,
  everything else exits with setup instructions.
- `chaos config export` prints JSON and `chaos config import` reads JSON.
- Administrator network constraints come only from
  `/etc/chaos/requirements.toml` (`[experimental_network]`).
- File-based `rules/*.decrees` and `agents/` come from two places only:
  `/etc/chaos` (rules may grant) and trusted project `.chaos` folders
  (restrictions only). The user's own rules and roles are database rows
  (`chaos rules`, `chaos agents`); `$CHAOS_HOME/rules` and
  `$CHAOS_HOME/agents` are not read.

### Added
- `chaos acp` exposes native Chaos sessions to ACP editors over stdio,
  including continued conversations, streamed text/tool activity, once-only
  approvals and cancellation. Existing provider accounts, project trust,
  instructions and sandbox policy remain kernel-owned.
- Independently publishable `acp-host` 0.1.0: bounded stable ACP v1 hosting
  over caller-owned Tokio streams using the official SDK, with no Chaos
  runtime dependencies.
- Editor MCP declarations select matching enabled Chaos registrations;
  unregistered commands or mismatching arguments/environment are rejected.
  Disconnect and uncertain cancellation fail closed without replaying work.
- `docs/getting-started.md`: database provisioning (PostgreSQL recommended),
  first run, headless bootstrap, first provider.
- `chaos agents list|show|import|export|remove|enable|disable`: agent role
  documents live in the runtime database. `$CHAOS_HOME/agents/` and
  `/etc/chaos/agents/` are no longer read; trusted project `.chaos/agents/`
  files still overlay stored roles.
- `chaos rules list|show|import|remove`: your exec-policy rules live in the
  runtime database as named documents and can only restrict; importing an
  `allow` decision is refused. `$CHAOS_HOME/rules/` is no longer read.
  Administrator `/etc/chaos/rules` and trusted project `.chaos/rules` are
  unchanged.
- `chaos modes` and `chaos prompts` (`list|show|import|export|remove|enable|
  disable`): custom collaboration modes and `/prompts:<name>` documents live
  in the runtime database. `$CHAOS_HOME/modes/` and `$CHAOS_HOME/prompts/`
  are no longer read. A stored mode that fails to parse is skipped with a
  warning instead of failing session start.
- `chaos themes` and `chaos scripts`: custom `.tmTheme` syntax themes and
  halluacinate Lua scripts live in the runtime database. `$CHAOS_HOME/themes/`
  and `~/.config/chaos/scripts/` are no longer read. Project `.chaos/scripts/`
  now load only when the project is trusted; previously any clone's scripts
  ran at session start.

### Fixed
- `chaos rules import`/`remove` bind the write to the settings revision read
  before the rules document, so two concurrent edits can no longer silently
  drop one another's restriction; the loser gets a revision conflict.
- `chaos themes import` validates with the tmTheme loader the console uses,
  so an import that succeeds is a theme that appears; a bad replacement
  leaves the stored theme untouched.
- Applying a stored agent role no longer drops the other stored roles from
  the resulting config, so nested agents can still select them.
- The message-history generation token now reads the SQLite file name from
  the open connection, so a storage URL outside ChaOS home still rejects
  stale history after the file is recreated.
- Compatible endpoints that accept one system block (self-hosted servers
  with strict chat templates) now receive every `developer`/`system`
  instruction hoisted into that block, in order, on both wire formats.
  47.13.2 relabeled instructions that arrived after the first user turn as
  `user` messages, so permission and mode changes reached the model as if
  the human had typed them.
- The workspace minimum Rust version remains 1.99, matching the toolchain
  pinned through `mise` and `rust-toolchain.toml`.
- PostgreSQL mounts now enforce the same per-partition log retention as
  SQLite; previously only time-based cleanup ran.
- The message-history log id on PostgreSQL changes when the table is
  rewritten, matching the SQLite inode semantics, so a stale client cursor
  cannot survive a reset.
- `chaos_journald` (SQLite) heartbeats and releases a lease inside one
  transaction; a stale owner can no longer extend or delete a lease another
  owner acquired in between.
- Machine status no longer probes free space on the SQLite directory when the
  mounted backend is PostgreSQL, and the model catalog cache logs the real
  backend instead of a filesystem path.

### Removed
- The evicted memory-consolidation and backfill tables' SQLite-only code in
  `chaos-proc` (no caller on either backend) and the unused `chaos-daemon`
  crate. Migration `0023` drops the orphaned `stage1_outputs`, `jobs`, and
  `backfill_state` tables on both backends.
- `Config.sqlite_home` is now `Config.storage_home`: a mount-registry key on
  every backend, the SQLite directory only under SQLite.

## [47.13.2] - 2026-10-11

### Added
- `chaos-local-models(7)` manual: self-hosted OpenAI-compatible servers (vLLM,
  Ollama, llama.cpp, LocalAI, SGLang, NIM, DGX Spark) as providers,
  with the structured tool-call switch for each backend.

### Fixed
- Keyless custom providers over `wire_api = "chat_completions"` no longer fail
  with "No credentials found for provider"; no `Authorization` header is sent
  when neither `env_key` nor a bearer token is configured.
- Self-hosted Responses and Chat Completions endpoints receive one leading
  system block: leading instruction items fold into it and later ones are sent
  as user messages, so strict chat templates (Qwen3.6/3.8 under vLLM) no longer
  reject requests with "System message must be at the beginning".

## [47.13.1] - 2026-10-10

### Added
- `chaos update` installs the latest GitHub release after confirmation,
  verifying its SHA-256 and the complete binary bundle before atomically
  replacing it. Only GitHub release builds include it; Nix, cargo, and source
  installs are refused. Package builds omit it unless `self-update` is enabled.

### Fixed
- Update the workspace to `mcp-host` 0.8.0 and remove redundant payload copies
  across MCP parsing, notifications, catalog/cache conversion, context
  distillation, and UI rendering. Reuse bounded stdio buffers without losing
  partial frames on cancelled reads.
- Share encoded provider payloads across retries, serialize WebSocket requests
  by borrowing, and avoid redundant history, Markdown, and journal allocations.
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

[Unreleased]: https://github.com/seuros/chaos/compare/v47.14.6...HEAD
[47.14.6]: https://github.com/seuros/chaos/compare/v47.14.5...v47.14.6
[47.14.1]: https://github.com/seuros/chaos/compare/v47.14.0...v47.14.1
[47.14.0]: https://github.com/seuros/chaos/compare/v47.13.2...v47.14.0
[47.13.2]: https://github.com/seuros/chaos/compare/v47.13.1...v47.13.2
[47.13.1]: https://github.com/seuros/chaos/compare/v47.13.0...v47.13.1
[47.13.0]: https://github.com/seuros/chaos/compare/v47.12.0...v47.13.0
[47.12.0]: https://github.com/seuros/chaos/compare/v47.11.0...v47.12.0
[47.10.0]: https://github.com/seuros/chaos/compare/v47.9.0...v47.10.0
[47.9.0]: https://github.com/seuros/chaos/compare/v47.8.0...v47.9.0
[47.8.0]: https://github.com/seuros/chaos/compare/v47.7.1...v47.8.0
[47.7.1]: https://github.com/seuros/chaos/compare/v47.7.0...v47.7.1
[47.7.0]: https://github.com/seuros/chaos/compare/v47.6.0...v47.7.0
[47.6.0]: https://github.com/seuros/chaos/releases/tag/v47.6.0
