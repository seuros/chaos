// Aggregates all former standalone integration tests as modules.
use std::ffi::OsString;

use chaos_argv::Arg0PathEntryGuard;
use chaos_argv::arg0_dispatch;
use ctor::ctor;
use tempfile::TempDir;

struct TestChaosAliasesGuard {
    _chaos_home: TempDir,
    _arg0: Arg0PathEntryGuard,
    _previous_chaos_home: Option<OsString>,
}

const CHAOS_HOME_ENV_VAR: &str = "CHAOS_HOME";

// This code runs before any other tests are run.
// It allows the test binary to behave like chaos and dispatch to apply_patch and alcatraz-linux
// based on the arg0.
// NOTE: this doesn't work on ARM
#[ctor]
pub static CHAOS_ALIASES_TEMP_DIR: TestChaosAliasesGuard = unsafe {
    #[allow(clippy::unwrap_used)]
    let chaos_home = tempfile::Builder::new()
        .prefix("chaos-tests")
        .tempdir()
        .unwrap();
    let previous_chaos_home = std::env::var_os(CHAOS_HOME_ENV_VAR);
    // arg0_dispatch() creates helper links under CHAOS_HOME/tmp. Point it at a
    // test-owned temp dir so startup never mutates the developer's real ~/.chaos.
    //
    // Safety: #[ctor] runs before tests start, so no test threads exist yet.
    unsafe {
        std::env::set_var(CHAOS_HOME_ENV_VAR, chaos_home.path());
    }

    #[allow(clippy::unwrap_used)]
    let arg0 = arg0_dispatch().unwrap();
    // Restore the process environment immediately so later tests observe the
    // same CHAOS_HOME state they started with.
    match previous_chaos_home.as_ref() {
        Some(value) => unsafe {
            std::env::set_var(CHAOS_HOME_ENV_VAR, value);
        },
        None => unsafe {
            std::env::remove_var(CHAOS_HOME_ENV_VAR);
        },
    }

    TestChaosAliasesGuard {
        _chaos_home: chaos_home,
        _arg0: arg0,
        _previous_chaos_home: previous_chaos_home,
    }
};

#[path = "abort_tasks.rs"]
mod abort_tasks;
#[path = "apply_patch_cli.rs"]
mod apply_patch_cli;
#[path = "approvals.rs"]
mod approvals;
#[path = "auth_refresh.rs"]
mod auth_refresh;
#[path = "chaos_delegate.rs"]
mod chaos_delegate;
#[path = "deprecation_notice.rs"]
mod deprecation_notice;
#[path = "exec.rs"]
mod exec;
#[path = "exec_policy.rs"]
mod exec_policy;
#[path = "grep_files.rs"]
mod grep_files;
#[path = "items.rs"]
mod items;
#[path = "json_result.rs"]
mod json_result;
#[path = "list_dir.rs"]
mod list_dir;
#[path = "live_cli.rs"]
mod live_cli;
#[path = "mcp_client.rs"]
mod mcp_client;
#[path = "minion_jobs.rs"]
mod minion_jobs;
#[path = "model_info_overrides.rs"]
mod model_info_overrides;
#[path = "model_overrides.rs"]
mod model_overrides;
#[path = "model_switching.rs"]
mod model_switching;
#[path = "models_cache_ttl.rs"]
mod models_cache_ttl;
#[path = "models_etag_responses.rs"]
mod models_etag_responses;
#[path = "otel.rs"]
mod otel;
#[path = "pending_input.rs"]
mod pending_input;
#[path = "personality.rs"]
mod personality;
#[path = "prompt_caching.rs"]
mod prompt_caching;
#[path = "quota_exceeded.rs"]
mod quota_exceeded;
#[path = "read_file.rs"]
mod read_file;
#[path = "remote_models.rs"]
mod remote_models;
#[path = "request_compression.rs"]
mod request_compression;
#[path = "request_permissions.rs"]
mod request_permissions;
#[path = "request_permissions_test_support.rs"]
mod request_permissions_test_support;
#[path = "request_permissions_tool.rs"]
mod request_permissions_tool;
#[path = "request_user_input.rs"]
mod request_user_input;
#[path = "safety_check_downgrade.rs"]
mod safety_check_downgrade;
#[path = "seatbelt.rs"]
mod seatbelt;
#[path = "shell_command.rs"]
mod shell_command;
#[path = "shell_serialization.rs"]
mod shell_serialization;
#[path = "shell_snapshot.rs"]
mod shell_snapshot;
#[path = "spawn_agent_description.rs"]
mod spawn_agent_description;
#[path = "stream_error_allows_next_turn.rs"]
mod stream_error_allows_next_turn;
#[path = "stream_no_completed.rs"]
mod stream_no_completed;
#[path = "text_encoding_fix.rs"]
mod text_encoding_fix;
#[path = "tool_harness.rs"]
mod tool_harness;
#[path = "tool_parallelism.rs"]
mod tool_parallelism;
#[path = "tools.rs"]
mod tools;
#[path = "truncation.rs"]
mod truncation;
#[path = "turn_state.rs"]
mod turn_state;
#[path = "undo.rs"]
mod undo;
#[path = "unified_exec.rs"]
mod unified_exec;

#[path = "user_notification.rs"]
mod user_notification;
#[path = "user_shell_cmd.rs"]
mod user_shell_cmd;
#[path = "view_image.rs"]
mod view_image;
#[path = "web_search.rs"]
mod web_search;
