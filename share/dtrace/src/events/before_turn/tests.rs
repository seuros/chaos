use std::path::PathBuf;

use chaos_ipc::protocol::HookEventName;
use chaos_ipc::protocol::HookOutputEntry;
use chaos_ipc::protocol::HookOutputEntryKind;
use chaos_ipc::protocol::HookRunStatus;
use pretty_assertions::assert_eq;

use super::BeforeTurnHandlerData;
use super::parse_completed;
use crate::engine::ConfiguredHandler;
use crate::engine::command_runner::CommandRunResult;

#[test]
fn plain_stdout_becomes_model_context() {
    let parsed = parse_completed(
        &handler(),
        run_result(Some(0), "remember this\n", ""),
        Some("turn-1".to_string()),
    );

    assert_eq!(
        parsed.data,
        BeforeTurnHandlerData {
            should_stop: false,
            stop_reason: None,
            additional_context_for_model: Some("remember this".to_string()),
        }
    );
    assert_eq!(parsed.completed.run.status, HookRunStatus::Completed);
    assert_eq!(
        parsed.completed.run.entries,
        vec![HookOutputEntry {
            kind: HookOutputEntryKind::Context,
            text: "remember this".to_string(),
        }]
    );
}

#[test]
fn continue_false_keeps_context_out_of_model_input() {
    let parsed = parse_completed(
        &handler(),
        run_result(
            Some(0),
            r#"{"continue":false,"stopReason":"pause","hookSpecificOutput":{"hookEventName":"BeforeTurn","additionalContext":"do not inject"}}"#,
            "",
        ),
        Some("turn-1".to_string()),
    );

    assert_eq!(
        parsed.data,
        BeforeTurnHandlerData {
            should_stop: true,
            stop_reason: Some("pause".to_string()),
            additional_context_for_model: None,
        }
    );
    assert_eq!(parsed.completed.run.status, HookRunStatus::Stopped);
}

fn handler() -> ConfiguredHandler {
    ConfiguredHandler {
        event_name: HookEventName::BeforeTurn,
        matcher: None,
        command: "hook".to_string(),
        timeout_sec: 5,
        status_message: None,
        source_path: PathBuf::from("/tmp/hooks.json"),
        display_order: 0,
    }
}

fn run_result(exit_code: Option<i32>, stdout: &str, stderr: &str) -> CommandRunResult {
    CommandRunResult {
        exit_code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        error: None,
        started_at: 100,
        completed_at: 150,
        duration_ms: 50,
    }
}
