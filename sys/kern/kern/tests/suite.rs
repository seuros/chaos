// Aggregates all former standalone integration tests as modules.
use std::ffi::OsString;

use chaos_argv::Arg0PathEntryGuard;
use chaos_argv::arg0_dispatch;
use ctor::ctor;
use tempfile::TempDir;

struct TestCodexAliasesGuard {
    _codex_home: TempDir,
    _arg0: Arg0PathEntryGuard,
    _previous_codex_home: Option<OsString>,
}

const CODEX_HOME_ENV_VAR: &str = "CODEX_HOME";

// This code runs before any other tests are run.
// It allows the test binary to behave like codex and dispatch to apply_patch and alcatraz-linux
// based on the arg0.
// NOTE: this doesn't work on ARM
#[ctor]
pub static CODEX_ALIASES_TEMP_DIR: TestCodexAliasesGuard = unsafe {
    #[allow(clippy::unwrap_used)]
    let codex_home = tempfile::Builder::new()
        .prefix("codex-core-tests")
        .tempdir()
        .unwrap();
    let previous_codex_home = std::env::var_os(CODEX_HOME_ENV_VAR);
    // arg0_dispatch() creates helper links under CODEX_HOME/tmp. Point it at a
    // test-owned temp dir so startup never mutates the developer's real ~/.codex.
    //
    // Safety: #[ctor] runs before tests start, so no test threads exist yet.
    unsafe {
        std::env::set_var(CODEX_HOME_ENV_VAR, codex_home.path());
    }

    #[allow(clippy::unwrap_used)]
    let arg0 = arg0_dispatch().unwrap();
    // Restore the process environment immediately so later tests observe the
    // same CODEX_HOME state they started with.
    match previous_codex_home.as_ref() {
        Some(value) => unsafe {
            std::env::set_var(CODEX_HOME_ENV_VAR, value);
        },
        None => unsafe {
            std::env::remove_var(CODEX_HOME_ENV_VAR);
        },
    }

    TestCodexAliasesGuard {
        _codex_home: codex_home,
        _arg0: arg0,
        _previous_codex_home: previous_codex_home,
    }
};

#[path = "suite/abort_tasks.rs"]
mod abort_tasks;
#[path = "suite/agent_jobs.rs"]
mod agent_jobs;
#[path = "suite/agent_websocket.rs"]
mod agent_websocket;
#[path = "suite/apply_patch_cli.rs"]
mod apply_patch_cli;
#[path = "suite/approvals.rs"]
mod approvals;
#[path = "suite/auth_refresh.rs"]
mod auth_refresh;
#[path = "suite/cli_stream.rs"]
mod cli_stream;
#[path = "suite/client.rs"]
mod client;
#[path = "suite/client_websockets.rs"]
mod client_websockets;
#[path = "suite/codex_delegate.rs"]
mod codex_delegate;
#[path = "suite/collaboration_instructions.rs"]
mod collaboration_instructions;
#[path = "suite/compact.rs"]
mod compact;
#[path = "suite/compact_remote.rs"]
mod compact_remote;
#[path = "suite/compact_resume_fork.rs"]
mod compact_resume_fork;
#[path = "suite/deprecation_notice.rs"]
mod deprecation_notice;
#[path = "suite/exec.rs"]
mod exec;
#[path = "suite/exec_policy.rs"]
mod exec_policy;
#[path = "suite/fork_process.rs"]
mod fork_process;
#[path = "suite/grep_files.rs"]
mod grep_files;
#[path = "suite/hierarchical_agents.rs"]
mod hierarchical_agents;
#[path = "suite/hooks.rs"]
mod hooks;
#[path = "suite/image_rollout.rs"]
mod image_rollout;
#[path = "suite/items.rs"]
mod items;
#[path = "suite/json_result.rs"]
mod json_result;
#[path = "suite/list_dir.rs"]
mod list_dir;
#[path = "suite/live_cli.rs"]
mod live_cli;
#[path = "suite/live_reload.rs"]
mod live_reload;
#[path = "suite/memories.rs"]
mod memories;
#[path = "suite/model_info_overrides.rs"]
mod model_info_overrides;
#[path = "suite/model_overrides.rs"]
mod model_overrides;
#[path = "suite/model_switching.rs"]
mod model_switching;
#[path = "suite/model_visible_layout.rs"]
mod model_visible_layout;
#[path = "suite/models_cache_ttl.rs"]
mod models_cache_ttl;
#[path = "suite/models_etag_responses.rs"]
mod models_etag_responses;
#[path = "suite/otel.rs"]
mod otel;
#[path = "suite/pending_input.rs"]
mod pending_input;
#[path = "suite/permissions_messages.rs"]
mod permissions_messages;
#[path = "suite/personality.rs"]
mod personality;
#[path = "suite/personality_migration.rs"]
mod personality_migration;
#[path = "suite/plugins.rs"]
mod plugins;
#[path = "suite/prompt_caching.rs"]
mod prompt_caching;
#[path = "suite/quota_exceeded.rs"]
mod quota_exceeded;
#[path = "suite/read_file.rs"]
mod read_file;
#[path = "suite/remote_models.rs"]
mod remote_models;
#[path = "suite/request_compression.rs"]
mod request_compression;
#[path = "suite/request_permissions.rs"]
mod request_permissions;
#[path = "suite/request_permissions_tool.rs"]
mod request_permissions_tool;
#[path = "suite/request_user_input.rs"]
mod request_user_input;
#[path = "suite/resume.rs"]
mod resume;
#[path = "suite/resume_warning.rs"]
mod resume_warning;
#[path = "suite/review.rs"]
mod review;
#[path = "suite/mcp_client.rs"]
mod mcp_client;
#[path = "suite/rollout_list_find.rs"]
mod rollout_list_find;
#[path = "suite/safety_check_downgrade.rs"]
mod safety_check_downgrade;
#[path = "suite/search_tool.rs"]
mod search_tool;
#[path = "suite/seatbelt.rs"]
mod seatbelt;
#[path = "suite/shell_command.rs"]
mod shell_command;
#[path = "suite/shell_serialization.rs"]
mod shell_serialization;
#[path = "suite/shell_snapshot.rs"]
mod shell_snapshot;
#[path = "suite/skills.rs"]
mod skills;
#[path = "suite/spawn_agent_description.rs"]
mod spawn_agent_description;
#[path = "suite/sqlite_state.rs"]
mod sqlite_state;
#[path = "suite/stream_error_allows_next_turn.rs"]
mod stream_error_allows_next_turn;
#[path = "suite/stream_no_completed.rs"]
mod stream_no_completed;
#[path = "suite/subagent_notifications.rs"]
mod subagent_notifications;
#[path = "suite/text_encoding_fix.rs"]
mod text_encoding_fix;
#[path = "suite/tool_harness.rs"]
mod tool_harness;
#[path = "suite/tool_parallelism.rs"]
mod tool_parallelism;
#[path = "suite/tools.rs"]
mod tools;
#[path = "suite/truncation.rs"]
mod truncation;
#[path = "suite/turn_state.rs"]
mod turn_state;
#[path = "suite/undo.rs"]
mod undo;
#[path = "suite/unified_exec.rs"]
mod unified_exec;
#[path = "suite/unstable_features_warning.rs"]
mod unstable_features_warning;
#[path = "suite/user_notification.rs"]
mod user_notification;
#[path = "suite/user_shell_cmd.rs"]
mod user_shell_cmd;
#[path = "suite/view_image.rs"]
mod view_image;
#[path = "suite/web_search.rs"]
mod web_search;
#[path = "suite/websocket_fallback.rs"]
mod websocket_fallback;
