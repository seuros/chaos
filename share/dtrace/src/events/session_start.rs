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
use crate::schema::SessionStartCommandInput;

#[derive(Debug, Clone, Copy)]
pub enum SessionStartSource {
    Startup,
    Resume,
}

impl SessionStartSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Resume => "resume",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionStartRequest {
    pub session_id: ProcessId,
    pub cwd: PathBuf,
    pub transcript_path: Option<PathBuf>,
    pub model: String,
    pub permission_mode: String,
    pub source: SessionStartSource,
}

#[derive(Debug)]
pub struct SessionStartOutcome {
    pub hook_events: Vec<HookCompletedEvent>,
    pub should_stop: bool,
    pub stop_reason: Option<String>,
    pub additional_context: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct SessionStartHandlerData {
    should_stop: bool,
    stop_reason: Option<String>,
    additional_context_for_model: Option<String>,
}

pub(crate) fn preview(
    handlers: &[ConfiguredHandler],
    request: &SessionStartRequest,
) -> Vec<HookRunSummary> {
    dispatcher::select_handlers(
        handlers,
        HookEventName::SessionStart,
        Some(request.source.as_str()),
    )
    .into_iter()
    .map(|handler| dispatcher::running_summary(&handler))
    .collect()
}

pub(crate) async fn run(
    handlers: &[ConfiguredHandler],
    shell: &CommandShell,
    request: SessionStartRequest,
    turn_id: Option<String>,
) -> SessionStartOutcome {
    let matched = dispatcher::select_handlers(
        handlers,
        HookEventName::SessionStart,
        Some(request.source.as_str()),
    );
    if matched.is_empty() {
        return SessionStartOutcome {
            hook_events: Vec::new(),
            should_stop: false,
            stop_reason: None,
            additional_context: None,
        };
    }

    let input_json = match serde_json::to_string(&SessionStartCommandInput::new(
        request.session_id.to_string(),
        request.transcript_path.clone(),
        request.cwd.display().to_string(),
        request.model.clone(),
        request.permission_mode.clone(),
        request.source.as_str().to_string(),
    )) {
        Ok(input_json) => input_json,
        Err(error) => {
            return serialization_failure_outcome(
                matched,
                turn_id,
                format!("failed to serialize session start hook input: {error}"),
            );
        }
    };

    let results = dispatcher::execute_handlers(
        shell,
        matched,
        input_json,
        request.cwd.as_path(),
        turn_id,
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

    SessionStartOutcome {
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
) -> dispatcher::ParsedHandler<SessionStartHandlerData> {
    let completion = crate::events::classify_universal_completion(
        &run_result,
        "hook returned invalid session start JSON output",
        |stdout| {
            output_parser::parse_session_start(stdout)
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
        data: SessionStartHandlerData {
            should_stop: completion.should_stop,
            stop_reason: completion.stop_reason,
            additional_context_for_model: completion.additional_context_for_model,
        },
    }
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
) -> SessionStartOutcome {
    SessionStartOutcome {
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
}
