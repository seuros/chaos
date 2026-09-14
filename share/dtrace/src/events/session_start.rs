use std::path::PathBuf;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::HookCompletedEvent;
use chaos_ipc::protocol::HookEventName;
use chaos_ipc::protocol::HookRunSummary;

use crate::HookAgentContext;
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
    pub agent_context: HookAgentContext,
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
        request.agent_context.clone(),
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
mod tests;
