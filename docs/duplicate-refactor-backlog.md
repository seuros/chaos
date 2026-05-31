# Duplicate-code refactor backlog

This backlog captures concrete duplicate or near-duplicate code found by parallel agent scans and a local duplicate-window scan. Items are ordered roughly by risk/reward: start with small, local consolidations, then move to cross-crate abstractions.

## Phase 1 — Small, local, low-risk refactors

- [x] Consolidate styled-line truncation helpers.
  - Files: `lib/libui/line_truncation.rs`, `lib/libui/status/format.rs`
  - Extract/reuse shared `line_width` / `truncate_line_to_width` helpers from `line_truncation.rs`.
  - Keep status-specific formatting in `status/format.rs`.

- [x] Consolidate `srv/pf` text response helpers.
  - Files: `srv/pf/src/responses.rs`, `srv/pf/src/http_proxy/responses.rs`
  - Remove the http-proxy-local `text_response` and use `crate::responses::text_response`.

- [x] Extract shared Arsenal MCP tool boilerplate.
  - Files: `srv/arsenal/src/tools/grep_files.rs`, `srv/arsenal/src/tools/list_dir.rs`, `srv/arsenal/src/tools/read_file.rs`
  - Add helpers for JSON argument deserialization and `Result<String, String>` to `ToolOutput` / `ToolError` mapping.

- [x] Extract cron owner-context construction.
  - Files: `srv/cron/src/tools/create.rs`, `srv/cron/src/tools/toggle.rs`, `srv/cron/src/tools/spool_submit.rs`
  - Add `OwnerContext::from_cron_ctx(...)` or `CronCtxExt::owner_context()`.

- [x] Extract cron storage-provider setup.
  - Files: `srv/cron/src/tools/create.rs`, `srv/cron/src/tools/toggle.rs`, `srv/cron/src/tools/spool_submit.rs`
  - Add shared provider/storage helper; keep spool registry validation separate.

## Phase 2 — UI bottom-pane consolidation

- [x] Extract shared bottom-pane composer draft.
  - Files: `lib/libui/bottom_pane/request_user_input.rs`, `lib/libui/bottom_pane/mcp_server_elicitation/domain.rs`
  - Move duplicate `ComposerDraft` and `text_with_pending` into `bottom_pane/composer_draft.rs`.

- [x] Extract shared footer-tip model and wrapping.
  - Files: `lib/libui/bottom_pane/request_user_input.rs`, `lib/libui/bottom_pane/mcp_server_elicitation/support.rs`
  - Add `bottom_pane/footer_tips.rs` with `FooterTip` and `wrap_footer_tips(...)`.

- [x] Extract shared footer-tip rendering.
  - Files: `lib/libui/bottom_pane/request_user_input/render.rs`, `lib/libui/bottom_pane/mcp_server_elicitation/ui.rs`
  - Add a shared renderer for footer rows, separator insertion, dim/highlight styling, and optional truncation.

- [x] Extract shared numbered-option helpers.
  - Files: `lib/libui/bottom_pane/request_user_input.rs`, `lib/libui/bottom_pane/mcp_server_elicitation/ui.rs`
  - Add helpers for option rows, required height, selected index, and digit shortcut parsing.

- [ ] Generalize word-boundary truncation.
  - Files: `lib/libui/bottom_pane/request_user_input/render.rs`, `lib/libui/line_truncation.rs`
  - Extend shared truncation with a boundary mode such as `Character` vs `Word`.

## Phase 3 — Kernel/tooling logic

- [x] Extract shared exec permission preparation.
  - Files: `sys/kern/kern/src/tools/handlers/shell.rs`, `sys/kern/kern/src/tools/handlers/unified_exec.rs`
  - Add `prepare_effective_exec_permissions(...)` or similar for approval policy, granted permissions, normalization, and escalation validation.

- [x] Deduplicate exec-policy amendment derivation.
  - File: `sys/kern/kern/src/exec_policy.rs`
  - Consolidate `try_derive_execpolicy_amendment_for_prompt_rules` and `try_derive_execpolicy_amendment_for_allow_rules` around a shared heuristics-amendment helper.

- [x] Deduplicate exec-policy reason derivation.
  - File: `sys/kern/kern/src/exec_policy.rs`
  - Extract `most_specific_prefix_match(evaluation, decision)` for `derive_prompt_reason` and `derive_forbidden_reason`.

- [ ] Extract common truncation primitives.
  - Files: `secure/watchdog/src/approval_request.rs`, `sys/kern/kern/src/truncate.rs`
  - Move approximate token/byte conversions and UTF-8-safe prefix/suffix splitting into a shared crate/module.

## Phase 4 — Platform sandbox/proxy consolidation

- [ ] Extract sandbox helper argument builder.
  - Files: `sys/kern/kern/src/landlock.rs`, `sys/arch/freebsd/src/capsicum.rs`
  - Move shared helper CLI flag construction into `alcatraz_base`.

- [ ] Extract loopback proxy environment parsing.
  - Files: `sys/arch/linux/src/linux_run_main.rs`, `sys/arch/macos/src/seatbelt.rs`
  - Add shared `loopback_proxy_ports(...)`, `is_loopback_host`, and `proxy_scheme_default_port` helpers.

- [ ] Deduplicate macOS seatbelt permission clause generation.
  - File: `sys/arch/macos/src/seatbelt_permissions.rs`
  - Encode read-write permissions as read clauses plus write clauses instead of repeating read-only clauses.

## Phase 5 — Service/API shared helpers

- [x] Extract MCP approval elicitation flow.
  - Files: `srv/mcpd/src/exec_approval.rs`, `srv/mcpd/src/patch_approval.rs`
  - Add generic helper for params construction, unsupported deny handling, response decode, and approval op submission.

- [ ] Consolidate Rama text/json response builders across services.
  - Files: `srv/journald/src/rama_http.rs`, `srv/pf/src/responses.rs`, `srv/pf/src/http_proxy/responses.rs`, `srv/httpd/src/api.rs`
  - Extract a small shared response utility if dependency boundaries allow; otherwise consolidate within each service first.

## Phase 6 — CLI/session lifecycle cleanup

- [ ] Share resume/fork CLI argument definitions.
  - File: `bin/chaos/src/main.rs`
  - Extract common `session_id`, `last`, `all`, and flattened `TuiCli` fields into a reusable args struct or action enum.

- [ ] Share resume/fork interactive finalization.
  - File: `bin/chaos/src/main.rs`
  - Extract `finalize_session_interactive(...)` with action-specific field assignment.

- [ ] Share ChatWidget initialization for resume/fork.
  - File: `bin/console/src/app/session_lifecycle.rs`
  - Extract `chat_widget_init_for_existing_process(...)` taking process, config, prompt/images, telemetry, and optional resumed session id.

## Phase 7 — Test fixture cleanup

- [ ] Extract common kernel test turn-submission helpers.
  - Files: `sys/kern/kern/tests/suite/stream_no_completed.rs`, `sys/kern/kern/tests/suite/otel.rs`, `sys/kern/kern/tests/suite/abort_tasks.rs`
  - Reduce repeated `Op::UserInput` setup and event waiting.

- [ ] Extract common unified-exec/view-image test setup.
  - Files: `sys/kern/kern/tests/suite/unified_exec.rs`, `sys/kern/kern/tests/suite/view_image.rs`
  - Reuse builder setup for `test_chaos`, cwd, session config, approval policy, and sandbox policy.

- [ ] Extract repeated MCP server config fixtures.
  - Files: `bin/chaos/tests/mcp_list.rs`, `bin/chaos/src/mcp_cmd.rs`, `var/proc/src/runtime.rs`, `sys/kern/kern/src/config/config_tests/mcp_servers.rs`, `sys/kern/kern/tests/suite/mcp_client.rs`
  - Add fixture/builder functions for default enabled MCP server config.

## Suggested execution order

1. `srv/pf` response helper consolidation.
2. `lib/libui` line truncation reuse.
3. `srv/arsenal` tool boilerplate helper.
4. `srv/cron` owner/storage helpers.
5. Bottom-pane shared `ComposerDraft` / `FooterTip` / option helpers.
6. Kernel exec-policy helper extraction.
7. Shell/unified-exec permission preparation helper.
8. Platform sandbox/proxy shared helpers.
9. MCP approval elicitation helper.
10. CLI resume/fork consolidation.
11. Test fixture cleanup.
