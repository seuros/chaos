use std::path::PathBuf;

use chaos_ipc::protocol::HookEventName;
use chaos_ipc::protocol::HookOutputEntry;
use chaos_ipc::protocol::HookOutputEntryKind;
use chaos_ipc::protocol::HookRunStatus;
use pretty_assertions::assert_eq;

use super::SessionStartHandlerData;
use super::parse_completed;
use crate::engine::ConfiguredHandler;
use crate::engine::command_runner::CommandRunResult;

#[test]
fn plain_stdout_becomes_model_context() {
    let parsed = parse_completed(
        &handler(),
        run_result(Some(0), "hello from hook\n", ""),
        None,
    );

    assert_eq!(
        parsed.data,
        SessionStartHandlerData {
            should_stop: false,
            stop_reason: None,
            additional_context_for_model: Some("hello from hook".to_string()),
        }
    );
    assert_eq!(parsed.completed.run.status, HookRunStatus::Completed);
    assert_eq!(
        parsed.completed.run.entries,
        vec![HookOutputEntry {
            kind: HookOutputEntryKind::Context,
            text: "hello from hook".to_string(),
        }]
    );
}

#[test]
fn continue_false_keeps_context_out_of_model_input() {
    let parsed = parse_completed(
        &handler(),
        run_result(
            Some(0),
            r#"{"continue":false,"stopReason":"pause","hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"do not inject"}}"#,
            "",
        ),
        None,
    );

    assert_eq!(
        parsed.data,
        SessionStartHandlerData {
            should_stop: true,
            stop_reason: Some("pause".to_string()),
            additional_context_for_model: None,
        }
    );
    assert_eq!(parsed.completed.run.status, HookRunStatus::Stopped);
}

#[test]
fn invalid_json_like_stdout_fails_instead_of_becoming_model_context() {
    let parsed = parse_completed(
        &handler(),
        run_result(
            Some(0),
            r#"{"hookSpecificOutput":{"hookEventName":"SessionStart""#,
            "",
        ),
        None,
    );

    assert_eq!(
        parsed.data,
        SessionStartHandlerData {
            should_stop: false,
            stop_reason: None,
            additional_context_for_model: None,
        }
    );
    assert_eq!(parsed.completed.run.status, HookRunStatus::Failed);
    assert_eq!(
        parsed.completed.run.entries,
        vec![HookOutputEntry {
            kind: HookOutputEntryKind::Error,
            text: "hook returned invalid session start JSON output".to_string(),
        }]
    );
}

fn handler() -> ConfiguredHandler {
    ConfiguredHandler {
        event_name: HookEventName::SessionStart,
        matcher: None,
        command: "echo hook".to_string(),
        timeout_sec: 600,
        status_message: None,
        source_path: PathBuf::from("/tmp/hooks.json"),
        display_order: 0,
    }
}

fn run_result(exit_code: Option<i32>, stdout: &str, stderr: &str) -> CommandRunResult {
    CommandRunResult {
        started_at: 1,
        completed_at: 2,
        duration_ms: 1,
        exit_code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        error: None,
    }
}
