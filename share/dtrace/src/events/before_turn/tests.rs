use std::path::PathBuf;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::HookEventName;
use chaos_ipc::protocol::HookOutputEntry;
use chaos_ipc::protocol::HookOutputEntryKind;
use chaos_ipc::protocol::HookRunStatus;
use pretty_assertions::assert_eq;

use super::BeforeTurnHandlerData;
use super::BeforeTurnRequest;
use super::parse_completed;
use super::run;
use crate::engine::CommandShell;
use crate::engine::ConfiguredHandler;
use crate::engine::command_runner::CommandRunResult;

fn shell() -> CommandShell {
    CommandShell {
        builder: None,
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string()],
        output_limit_bytes: crate::engine::command_runner::DEFAULT_OUTPUT_LIMIT_BYTES,
    }
}

fn integration_handler(command: &str) -> ConfiguredHandler {
    ConfiguredHandler {
        event_name: HookEventName::BeforeTurn,
        matcher: None,
        command: command.to_string(),
        timeout_sec: 10,
        status_message: None,
        source_path: PathBuf::from("/tmp/hooks.json"),
        display_order: 0,
    }
}

fn request() -> BeforeTurnRequest {
    BeforeTurnRequest {
        session_id: ProcessId::default(),
        turn_id: "turn-1".to_string(),
        cwd: std::env::temp_dir(),
        transcript_path: None,
        model: "test-model".to_string(),
        permission_mode: "default".to_string(),
        input_messages: vec!["hello".to_string()],
        agent_context: crate::HookAgentContext::default(),
    }
}

// Subprocess-driven tests exercise JSON serialization, real shell invocation,
// stdout parsing, and outcome aggregation through the full before-turn path.
// A valid JSON payload lifts `additionalContext` into the outcome so the turn
// driver can inject it as developer instructions before the model samples.
#[tokio::test]
async fn json_context_payload_surfaces_as_additional_context() {
    let handlers = vec![integration_handler(
        r#"cat >/dev/null; printf '%s' '{"continue":true,"hookSpecificOutput":{"hookEventName":"BeforeTurn","additionalContext":"remember this"}}'"#,
    )];

    let outcome = run(&handlers, &shell(), request()).await;

    assert_eq!(outcome.additional_context.as_deref(), Some("remember this"));
    assert!(!outcome.should_stop);
    assert_eq!(outcome.hook_events.len(), 1);
    assert_eq!(outcome.hook_events[0].run.status, HookRunStatus::Completed);
}

// A `continue:false` payload short-circuits the turn before sampling and
// suppresses any `additionalContext` so the model never sees it.
#[tokio::test]
async fn continue_false_short_circuits_turn() {
    let handlers = vec![integration_handler(
        r#"cat >/dev/null; printf '%s' '{"continue":false,"stopReason":"pause","hookSpecificOutput":{"hookEventName":"BeforeTurn","additionalContext":"do not inject"}}'"#,
    )];

    let outcome = run(&handlers, &shell(), request()).await;

    assert!(outcome.should_stop);
    assert_eq!(outcome.stop_reason.as_deref(), Some("pause"));
    assert!(outcome.additional_context.is_none());
    assert_eq!(outcome.hook_events[0].run.status, HookRunStatus::Stopped);
}

// Plain-text stdout is treated as raw context so simple hook scripts can
// inject reminders without learning the wire schema.
#[tokio::test]
async fn plain_text_stdout_becomes_additional_context() {
    let handlers = vec![integration_handler(
        r#"cat >/dev/null; printf 'remember this\n'"#,
    )];

    let outcome = run(&handlers, &shell(), request()).await;

    assert_eq!(outcome.additional_context.as_deref(), Some("remember this"));
    assert!(!outcome.should_stop);
}

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
