use std::path::PathBuf;

use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::HookCompletedEvent;
use chaos_ipc::protocol::HookEventName;
use chaos_ipc::protocol::HookExecutionMode;
use chaos_ipc::protocol::HookHandlerType;
use chaos_ipc::protocol::HookOutputEntry;
use chaos_ipc::protocol::HookRunStatus;
use chaos_ipc::protocol::HookRunSummary;
use chaos_ipc::protocol::HookScope;
use chaos_ipc::protocol::HookStartedEvent;

use super::EventProcessorWithHumanOutput;
use super::helpers::should_print_final_message_to_stdout;
use pretty_assertions::assert_eq;

#[test]
fn suppresses_final_stdout_message_when_both_streams_are_terminals() {
    assert_eq!(
        should_print_final_message_to_stdout(Some("hello"), true, true),
        false
    );
}

#[test]
fn prints_final_stdout_message_when_stdout_is_not_terminal() {
    assert_eq!(
        should_print_final_message_to_stdout(Some("hello"), false, true),
        true
    );
}

#[test]
fn prints_final_stdout_message_when_stderr_is_not_terminal() {
    assert_eq!(
        should_print_final_message_to_stdout(Some("hello"), true, false),
        true
    );
}

#[test]
fn does_not_print_when_message_is_missing() {
    assert_eq!(
        should_print_final_message_to_stdout(None, false, false),
        false
    );
}

#[test]
fn hook_started_with_status_message_is_not_silent() {
    let event = HookStartedEvent {
        turn_id: Some("turn-1".to_string()),
        run: hook_run(
            HookRunStatus::Running,
            Some("running hook"),
            Vec::new(),
            HookEventName::Stop,
        ),
    };

    assert!(!EventProcessorWithHumanOutput::is_silent_event(
        &EventMsg::HookStarted(event)
    ));
}

#[test]
fn hook_completed_failure_interrupts_progress() {
    let event = HookCompletedEvent {
        turn_id: Some("turn-1".to_string()),
        run: hook_run(HookRunStatus::Failed, None, Vec::new(), HookEventName::Stop),
    };

    assert!(EventProcessorWithHumanOutput::should_interrupt_progress(
        &EventMsg::HookCompleted(event)
    ));
}

#[test]
fn hook_completed_success_without_entries_stays_silent() {
    let event = HookCompletedEvent {
        turn_id: Some("turn-1".to_string()),
        run: hook_run(
            HookRunStatus::Completed,
            None,
            Vec::new(),
            HookEventName::Stop,
        ),
    };

    assert!(EventProcessorWithHumanOutput::is_silent_event(
        &EventMsg::HookCompleted(event)
    ));
}

fn hook_run(
    status: HookRunStatus,
    status_message: Option<&str>,
    entries: Vec<HookOutputEntry>,
    event_name: HookEventName,
) -> HookRunSummary {
    HookRunSummary {
        id: "hook-run-1".to_string(),
        event_name,
        handler_type: HookHandlerType::Command,
        execution_mode: HookExecutionMode::Sync,
        scope: HookScope::Turn,
        source_path: PathBuf::from("/tmp/hooks.json"),
        display_order: 0,
        status,
        status_message: status_message.map(ToOwned::to_owned),
        started_at: 0,
        completed_at: Some(1),
        duration_ms: Some(1),
        entries,
    }
}
