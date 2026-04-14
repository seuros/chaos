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

#[path = "suite/abort_tasks.rs"]
mod abort_tasks;
#[path = "suite/agent_jobs.rs"]
mod agent_jobs;
#[path = "suite/apply_patch_cli.rs"]
mod apply_patch_cli;
#[path = "suite/approvals.rs"]
mod approvals;
#[path = "suite/auth_refresh.rs"]
mod auth_refresh;
#[path = "suite/chaos_delegate.rs"]
mod chaos_delegate;
#[path = "suite/deprecation_notice.rs"]
mod deprecation_notice;
#[path = "suite/exec.rs"]
mod exec;
#[path = "suite/exec_policy.rs"]
mod exec_policy;
#[path = "suite/grep_files.rs"]
mod grep_files;
#[path = "suite/items.rs"]
mod items;
#[path = "suite/json_result.rs"]
mod json_result;
#[path = "suite/list_dir.rs"]
mod list_dir;
#[path = "suite/live_cli.rs"]
mod live_cli;
#[path = "suite/mcp_client.rs"]
mod mcp_client;
#[path = "suite/model_info_overrides.rs"]
mod model_info_overrides;
#[path = "suite/model_overrides.rs"]
mod model_overrides;
#[path = "suite/model_switching.rs"]
mod model_switching;
#[path = "suite/models_cache_ttl.rs"]
mod models_cache_ttl;
#[path = "suite/models_etag_responses.rs"]
mod models_etag_responses;
#[path = "suite/otel.rs"]
mod otel;
#[path = "suite/pending_input.rs"]
mod pending_input;
#[path = "suite/personality.rs"]
mod personality;
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
#[path = "suite/safety_check_downgrade.rs"]
mod safety_check_downgrade;
#[path = "suite/seatbelt.rs"]
mod seatbelt;
#[path = "suite/shell_command.rs"]
mod shell_command;
#[path = "suite/shell_serialization.rs"]
mod shell_serialization;
#[path = "suite/shell_snapshot.rs"]
mod shell_snapshot;
#[path = "suite/spawn_agent_description.rs"]
mod spawn_agent_description;
#[path = "suite/stream_error_allows_next_turn.rs"]
mod stream_error_allows_next_turn;
#[path = "suite/stream_no_completed.rs"]
mod stream_no_completed;
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

#[path = "suite/user_notification.rs"]
mod user_notification;
#[path = "suite/user_shell_cmd.rs"]
mod user_shell_cmd;
#[path = "suite/view_image.rs"]
mod view_image;
#[path = "suite/web_search.rs"]
mod web_search;
