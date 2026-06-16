use std::path::PathBuf;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::HookCompletedEvent;
use chaos_ipc::protocol::HookEventName;
use chaos_ipc::protocol::HookRunSummary;

use crate::engine::CommandShell;
use crate::engine::ConfiguredHandler;
use crate::engine::command_runner::CommandRunResult;
use crate::engine::dispatcher;
use crate::engine::output_parser;
use crate::schema::BeforeTurnCommandInput;

#[derive(Debug, Clone)]
pub struct BeforeTurnRequest {
    pub session_id: ProcessId,
    pub turn_id: String,
    pub cwd: PathBuf,
    pub transcript_path: Option<PathBuf>,
    pub model: String,
    pub permission_mode: String,
    pub input_messages: Vec<String>,
}

#[derive(Debug)]
pub struct BeforeTurnOutcome {
    pub hook_events: Vec<HookCompletedEvent>,
    pub should_stop: bool,
    pub stop_reason: Option<String>,
    pub additional_context: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct BeforeTurnHandlerData {
    should_stop: bool,
    stop_reason: Option<String>,
    additional_context_for_model: Option<String>,
}

pub(crate) fn preview(
    handlers: &[ConfiguredHandler],
    request: &BeforeTurnRequest,
) -> Vec<HookRunSummary> {
    let matcher_input = matcher_input(&request.input_messages);
    dispatcher::select_handlers(handlers, HookEventName::BeforeTurn, Some(&matcher_input))
        .into_iter()
        .map(|handler| dispatcher::running_summary(&handler))
        .collect()
}

pub(crate) async fn run(
    handlers: &[ConfiguredHandler],
    shell: &CommandShell,
    request: BeforeTurnRequest,
) -> BeforeTurnOutcome {
    let matcher_input = matcher_input(&request.input_messages);
    let matched = dispatcher::select_handlers(
        handlers,
        HookEventName::BeforeTurn,
        Some(matcher_input.as_str()),
    );
    if matched.is_empty() {
        return BeforeTurnOutcome {
            hook_events: Vec::new(),
            should_stop: false,
            stop_reason: None,
            additional_context: None,
        };
    }

    let input_json = match serde_json::to_string(&BeforeTurnCommandInput::new(
        request.session_id.to_string(),
        request.transcript_path.clone(),
        request.cwd.display().to_string(),
        request.turn_id.clone(),
        request.model.clone(),
        request.permission_mode.clone(),
        request.input_messages.clone(),
    )) {
        Ok(input_json) => input_json,
        Err(error) => {
            return serialization_failure_outcome(
                matched,
                Some(request.turn_id),
                format!("failed to serialize before turn hook input: {error}"),
            );
        }
    };

    let results = dispatcher::execute_handlers(
        shell,
        matched,
        input_json,
        request.cwd.as_path(),
        Some(request.turn_id),
        parse_completed,
    )
    .await;

    let should_stop = results.iter().any(|result| result.data.should_stop);
    let stop_reason = results
        .iter()
        .find_map(|result| result.data.stop_reason.clone());
    let additional_contexts = results
        .iter()
        .filter_map(|result| result.data.additional_context_for_model.clone())
        .collect::<Vec<_>>();

    BeforeTurnOutcome {
        hook_events: results.into_iter().map(|result| result.completed).collect(),
        should_stop,
        stop_reason,
        additional_context: join_text_chunks(additional_contexts),
    }
}

fn parse_completed(
    handler: &ConfiguredHandler,
    run_result: CommandRunResult,
    turn_id: Option<String>,
) -> dispatcher::ParsedHandler<BeforeTurnHandlerData> {
    let completion = crate::events::classify_universal_completion(
        &run_result,
        "hook returned invalid before turn JSON output",
        |stdout| {
            output_parser::parse_before_turn(stdout)
                .map(|parsed| (parsed.universal, parsed.additional_context))
        },
    );

    let completed = HookCompletedEvent {
        turn_id,
        run: dispatcher::completed_summary(
            handler,
            &run_result,
            completion.status,
            completion.entries,
        ),
    };

    dispatcher::ParsedHandler {
        completed,
        data: BeforeTurnHandlerData {
            should_stop: completion.should_stop,
            stop_reason: completion.stop_reason,
            additional_context_for_model: completion.additional_context_for_model,
        },
    }
}

fn matcher_input(input_messages: &[String]) -> String {
    input_messages.join("\n")
}

fn join_text_chunks(chunks: Vec<String>) -> Option<String> {
    if chunks.is_empty() {
        None
    } else {
        Some(chunks.join("\n\n"))
    }
}

fn serialization_failure_outcome(
    handlers: Vec<ConfiguredHandler>,
    turn_id: Option<String>,
    error_message: String,
) -> BeforeTurnOutcome {
    BeforeTurnOutcome {
        hook_events: crate::events::failed_hook_events(handlers, turn_id, error_message),
        should_stop: false,
        stop_reason: None,
        additional_context: None,
    }
}

#[cfg(test)]
mod tests {
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
}

/// Subprocess-driven integration tests that exercise the full
/// `run_before_turn` path: JSON serialization of the input, real shell
/// invocation, stdout parsing, and outcome aggregation.
#[cfg(test)]
mod integration_tests {
    use std::path::PathBuf;

    use chaos_ipc::ProcessId;
    use chaos_ipc::protocol::HookEventName;
    use chaos_ipc::protocol::HookRunStatus;
    use pretty_assertions::assert_eq;

    use super::BeforeTurnRequest;
    use super::run;
    use crate::engine::CommandShell;
    use crate::engine::ConfiguredHandler;

    fn shell() -> CommandShell {
        CommandShell {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string()],
        }
    }

    fn handler(command: &str) -> ConfiguredHandler {
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
        }
    }

    // A valid JSON payload from a hook script lifts `additionalContext` into
    // the outcome so the turn driver can inject it as developer instructions
    // before the model samples.
    #[tokio::test]
    async fn json_context_payload_surfaces_as_additional_context() {
        let handlers = vec![handler(
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
        let handlers = vec![handler(
            r#"cat >/dev/null; printf '%s' '{"continue":false,"stopReason":"pause","hookSpecificOutput":{"hookEventName":"BeforeTurn","additionalContext":"do not inject"}}'"#,
        )];

        let outcome = run(&handlers, &shell(), request()).await;

        assert!(outcome.should_stop);
        assert_eq!(outcome.stop_reason.as_deref(), Some("pause"));
        assert!(outcome.additional_context.is_none());
        assert_eq!(outcome.hook_events[0].run.status, HookRunStatus::Stopped);
    }

    // Plain-text stdout (no JSON wrapper) is treated as raw context so simple
    // hook scripts can inject reminders without learning the wire schema.
    #[tokio::test]
    async fn plain_text_stdout_becomes_additional_context() {
        let handlers = vec![handler(r#"cat >/dev/null; printf 'remember this\n'"#)];

        let outcome = run(&handlers, &shell(), request()).await;

        assert_eq!(outcome.additional_context.as_deref(), Some("remember this"));
        assert!(!outcome.should_stop);
    }
}
