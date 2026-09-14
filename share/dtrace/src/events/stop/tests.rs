use std::path::PathBuf;

use chaos_ipc::protocol::HookEventName;
use chaos_ipc::protocol::HookOutputEntry;
use chaos_ipc::protocol::HookOutputEntryKind;
use chaos_ipc::protocol::HookRunStatus;
use pretty_assertions::assert_eq;

use super::StopHandlerData;
use super::aggregate_results;
use super::parse_completed;
use crate::engine::ConfiguredHandler;
use crate::engine::command_runner::CommandRunResult;

#[test]
fn block_decision_with_reason_sets_continuation_prompt() {
    let parsed = parse_completed(
        &handler(),
        run_result(
            Some(0),
            r#"{"decision":"block","reason":"retry with tests"}"#,
            "",
        ),
        Some("turn-1".to_string()),
    );

    assert_eq!(
        parsed.data,
        StopHandlerData {
            should_stop: false,
            stop_reason: None,
            should_block: true,
            block_reason: Some("retry with tests".to_string()),
            continuation_prompt: Some("retry with tests".to_string()),
        }
    );
    assert_eq!(parsed.completed.run.status, HookRunStatus::Blocked);
}

#[test]
fn block_decision_without_reason_is_invalid() {
    let parsed = parse_completed(
        &handler(),
        run_result(Some(0), r#"{"decision":"block"}"#, ""),
        Some("turn-1".to_string()),
    );

    assert_eq!(parsed.data, StopHandlerData::default());
    assert_eq!(parsed.completed.run.status, HookRunStatus::Failed);
    assert_eq!(
        parsed.completed.run.entries,
        vec![HookOutputEntry {
            kind: HookOutputEntryKind::Error,
            text: "Stop hook returned decision:block without a non-empty reason".to_string(),
        }]
    );
}

#[test]
fn continue_false_overrides_block_decision() {
    let parsed = parse_completed(
        &handler(),
        run_result(
            Some(0),
            r#"{"continue":false,"stopReason":"done","decision":"block","reason":"keep going"}"#,
            "",
        ),
        Some("turn-1".to_string()),
    );

    assert_eq!(
        parsed.data,
        StopHandlerData {
            should_stop: true,
            stop_reason: Some("done".to_string()),
            should_block: false,
            block_reason: None,
            continuation_prompt: None,
        }
    );
    assert_eq!(parsed.completed.run.status, HookRunStatus::Stopped);
}

#[test]
fn exit_code_two_uses_stderr_feedback_only() {
    let parsed = parse_completed(
        &handler(),
        run_result(Some(2), "ignored stdout", "retry with tests"),
        Some("turn-1".to_string()),
    );

    assert_eq!(
        parsed.data,
        StopHandlerData {
            should_stop: false,
            stop_reason: None,
            should_block: true,
            block_reason: Some("retry with tests".to_string()),
            continuation_prompt: Some("retry with tests".to_string()),
        }
    );
    assert_eq!(parsed.completed.run.status, HookRunStatus::Blocked);
}

#[test]
fn exit_code_two_without_stderr_does_not_block() {
    let parsed = parse_completed(&handler(), run_result(Some(2), "", "   "), None);

    assert_eq!(parsed.data, StopHandlerData::default());
    assert_eq!(parsed.completed.run.status, HookRunStatus::Failed);
    assert_eq!(
        parsed.completed.run.entries,
        vec![HookOutputEntry {
            kind: HookOutputEntryKind::Error,
            text: "Stop hook exited with code 2 but did not write a continuation prompt to stderr"
                .to_string(),
        }]
    );
}

#[test]
fn block_decision_with_blank_reason_fails_instead_of_blocking() {
    let parsed = parse_completed(
        &handler(),
        run_result(Some(0), "{\"decision\":\"block\",\"reason\":\"   \"}", ""),
        Some("turn-1".to_string()),
    );

    assert_eq!(parsed.data, StopHandlerData::default());
    assert_eq!(parsed.completed.run.status, HookRunStatus::Failed);
    assert_eq!(
        parsed.completed.run.entries,
        vec![HookOutputEntry {
            kind: HookOutputEntryKind::Error,
            text: "Stop hook returned decision:block without a non-empty reason".to_string(),
        }]
    );
}

#[test]
fn invalid_stdout_fails_instead_of_silently_nooping() {
    let parsed = parse_completed(
        &handler(),
        run_result(Some(0), "not json", ""),
        Some("turn-1".to_string()),
    );

    assert_eq!(parsed.data, StopHandlerData::default());
    assert_eq!(parsed.completed.run.status, HookRunStatus::Failed);
    assert_eq!(
        parsed.completed.run.entries,
        vec![HookOutputEntry {
            kind: HookOutputEntryKind::Error,
            text: "hook returned invalid stop hook JSON output".to_string(),
        }]
    );
}

#[test]
fn aggregate_results_concatenates_blocking_reasons_in_declaration_order() {
    let aggregate = aggregate_results([
        &StopHandlerData {
            should_stop: false,
            stop_reason: None,
            should_block: true,
            block_reason: Some("first".to_string()),
            continuation_prompt: Some("first".to_string()),
        },
        &StopHandlerData {
            should_stop: false,
            stop_reason: None,
            should_block: true,
            block_reason: Some("second".to_string()),
            continuation_prompt: Some("second".to_string()),
        },
    ]);

    assert_eq!(
        aggregate,
        StopHandlerData {
            should_stop: false,
            stop_reason: None,
            should_block: true,
            block_reason: Some("first\n\nsecond".to_string()),
            continuation_prompt: Some("first\n\nsecond".to_string()),
        }
    );
}

fn handler() -> ConfiguredHandler {
    ConfiguredHandler {
        event_name: HookEventName::Stop,
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
