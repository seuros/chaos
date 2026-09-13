use std::path::PathBuf;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::HookCompletedEvent;
use chaos_ipc::protocol::HookEventName;
use chaos_ipc::protocol::HookOutputEntry;
use chaos_ipc::protocol::HookOutputEntryKind;
use chaos_ipc::protocol::HookRunStatus;
use chaos_ipc::protocol::HookRunSummary;

use crate::HookAgentContext;
use crate::engine::CommandShell;
use crate::engine::ConfiguredHandler;
use crate::engine::command_runner::CommandRunResult;
use crate::engine::dispatcher;
use crate::engine::output_parser;
use crate::schema::StopCommandInput;

#[derive(Debug, Clone)]
pub struct StopRequest {
    pub session_id: ProcessId,
    pub turn_id: String,
    pub cwd: PathBuf,
    pub transcript_path: Option<PathBuf>,
    pub model: String,
    pub permission_mode: String,
    pub stop_hook_active: bool,
    pub last_assistant_message: Option<String>,
    pub agent_context: HookAgentContext,
}

#[derive(Debug)]
pub struct StopOutcome {
    pub hook_events: Vec<HookCompletedEvent>,
    pub should_stop: bool,
    pub stop_reason: Option<String>,
    pub should_block: bool,
    pub block_reason: Option<String>,
    pub continuation_prompt: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct StopHandlerData {
    should_stop: bool,
    stop_reason: Option<String>,
    should_block: bool,
    block_reason: Option<String>,
    continuation_prompt: Option<String>,
}

pub(crate) fn preview(
    handlers: &[ConfiguredHandler],
    _request: &StopRequest,
) -> Vec<HookRunSummary> {
    dispatcher::select_handlers(handlers, HookEventName::Stop, /*matcher_input*/ None)
        .into_iter()
        .map(|handler| dispatcher::running_summary(&handler))
        .collect()
}

pub(crate) async fn run(
    handlers: &[ConfiguredHandler],
    shell: &CommandShell,
    request: StopRequest,
) -> StopOutcome {
    let matched =
        dispatcher::select_handlers(handlers, HookEventName::Stop, /*matcher_input*/ None);
    if matched.is_empty() {
        return StopOutcome {
            hook_events: Vec::new(),
            should_stop: false,
            stop_reason: None,
            should_block: false,
            block_reason: None,
            continuation_prompt: None,
        };
    }

    let input_json = match serde_json::to_string(&StopCommandInput::new(
        request.session_id.to_string(),
        request.transcript_path.clone(),
        request.cwd.display().to_string(),
        request.model.clone(),
        request.permission_mode.clone(),
        request.stop_hook_active,
        request.last_assistant_message.clone(),
        request.agent_context.clone(),
    )) {
        Ok(input_json) => input_json,
        Err(error) => {
            return serialization_failure_outcome(
                matched,
                Some(request.turn_id),
                format!("failed to serialize stop hook input: {error}"),
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

    let aggregate = aggregate_results(results.iter().map(|result| &result.data));

    StopOutcome {
        hook_events: results.into_iter().map(|result| result.completed).collect(),
        should_stop: aggregate.should_stop,
        stop_reason: aggregate.stop_reason,
        should_block: aggregate.should_block,
        block_reason: aggregate.block_reason,
        continuation_prompt: aggregate.continuation_prompt,
    }
}

fn parse_completed(
    handler: &ConfiguredHandler,
    run_result: CommandRunResult,
    turn_id: Option<String>,
) -> dispatcher::ParsedHandler<StopHandlerData> {
    let mut entries = Vec::new();
    let mut status = HookRunStatus::Completed;
    let mut should_stop = false;
    let mut stop_reason = None;
    let mut should_block = false;
    let mut block_reason = None;
    let mut continuation_prompt = None;

    match run_result.error.as_deref() {
        Some(error) => {
            status = HookRunStatus::Failed;
            entries.push(HookOutputEntry {
                kind: HookOutputEntryKind::Error,
                text: error.to_string(),
            });
        }
        None => match run_result.exit_code {
            Some(0) => {
                let trimmed_stdout = run_result.stdout.trim();
                if trimmed_stdout.is_empty() {
                } else if let Some(parsed) = output_parser::parse_stop(&run_result.stdout) {
                    if let Some(system_message) = parsed.universal.system_message {
                        entries.push(HookOutputEntry {
                            kind: HookOutputEntryKind::Warning,
                            text: system_message,
                        });
                    }
                    let _ = parsed.universal.suppress_output;
                    if !parsed.universal.continue_processing {
                        status = HookRunStatus::Stopped;
                        should_stop = true;
                        stop_reason = parsed.universal.stop_reason.clone();
                        if let Some(stop_reason_text) = parsed.universal.stop_reason {
                            entries.push(HookOutputEntry {
                                kind: HookOutputEntryKind::Stop,
                                text: stop_reason_text,
                            });
                        }
                    } else if let Some(invalid_block_reason) = parsed.invalid_block_reason {
                        status = HookRunStatus::Failed;
                        entries.push(HookOutputEntry {
                            kind: HookOutputEntryKind::Error,
                            text: invalid_block_reason,
                        });
                    } else if parsed.should_block {
                        if let Some(reason) = parsed.reason.as_deref().and_then(trimmed_non_empty) {
                            status = HookRunStatus::Blocked;
                            should_block = true;
                            block_reason = Some(reason.clone());
                            continuation_prompt = Some(reason.clone());
                            entries.push(HookOutputEntry {
                                kind: HookOutputEntryKind::Feedback,
                                text: reason,
                            });
                        } else {
                            status = HookRunStatus::Failed;
                            entries.push(HookOutputEntry {
                                kind: HookOutputEntryKind::Error,
                                text:
                                    "Stop hook returned decision:block without a non-empty reason"
                                        .to_string(),
                            });
                        }
                    }
                } else {
                    status = HookRunStatus::Failed;
                    entries.push(HookOutputEntry {
                        kind: HookOutputEntryKind::Error,
                        text: "hook returned invalid stop hook JSON output".to_string(),
                    });
                }
            }
            Some(2) => {
                if let Some(reason) = trimmed_non_empty(&run_result.stderr) {
                    status = HookRunStatus::Blocked;
                    should_block = true;
                    block_reason = Some(reason.clone());
                    continuation_prompt = Some(reason.clone());
                    entries.push(HookOutputEntry {
                        kind: HookOutputEntryKind::Feedback,
                        text: reason,
                    });
                } else {
                    status = HookRunStatus::Failed;
                    entries.push(HookOutputEntry {
                        kind: HookOutputEntryKind::Error,
                        text:
                            "Stop hook exited with code 2 but did not write a continuation prompt to stderr"
                                .to_string(),
                    });
                }
            }
            Some(exit_code) => {
                status = HookRunStatus::Failed;
                entries.push(HookOutputEntry {
                    kind: HookOutputEntryKind::Error,
                    text: format!("hook exited with code {exit_code}"),
                });
            }
            None => {
                status = HookRunStatus::Failed;
                entries.push(HookOutputEntry {
                    kind: HookOutputEntryKind::Error,
                    text: "hook exited without a status code".to_string(),
                });
            }
        },
    }

    let completed = HookCompletedEvent {
        turn_id,
        run: dispatcher::completed_summary(handler, &run_result, status, entries),
    };

    dispatcher::ParsedHandler {
        completed,
        data: StopHandlerData {
            should_stop,
            stop_reason,
            should_block,
            block_reason,
            continuation_prompt,
        },
    }
}

fn aggregate_results<'a>(
    results: impl IntoIterator<Item = &'a StopHandlerData>,
) -> StopHandlerData {
    let results = results.into_iter().collect::<Vec<_>>();
    let should_stop = results.iter().any(|result| result.should_stop);
    let stop_reason = results.iter().find_map(|result| result.stop_reason.clone());
    let should_block = !should_stop && results.iter().any(|result| result.should_block);
    let block_reason = if should_block {
        join_block_text(results.iter().copied(), |result| {
            result.block_reason.as_deref()
        })
    } else {
        None
    };
    let continuation_prompt = if should_block {
        join_block_text(results.iter().copied(), |result| {
            result.continuation_prompt.as_deref()
        })
    } else {
        None
    };

    StopHandlerData {
        should_stop,
        stop_reason,
        should_block,
        block_reason,
        continuation_prompt,
    }
}

fn join_block_text<'a>(
    results: impl IntoIterator<Item = &'a StopHandlerData>,
    select: impl Fn(&'a StopHandlerData) -> Option<&'a str>,
) -> Option<String> {
    let parts = results
        .into_iter()
        .filter_map(select)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\n\n"))
}

fn trimmed_non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        return Some(trimmed.to_string());
    }
    None
}

fn serialization_failure_outcome(
    handlers: Vec<ConfiguredHandler>,
    turn_id: Option<String>,
    error_message: String,
) -> StopOutcome {
    StopOutcome {
        hook_events: crate::events::failed_hook_events(handlers, turn_id, error_message),
        should_stop: false,
        stop_reason: None,
        should_block: false,
        block_reason: None,
        continuation_prompt: None,
    }
}

#[cfg(test)]
mod tests;
