pub mod before_turn;
pub mod session_start;
pub mod stop;

use chaos_ipc::protocol::HookCompletedEvent;
use chaos_ipc::protocol::HookOutputEntry;
use chaos_ipc::protocol::HookOutputEntryKind;
use chaos_ipc::protocol::HookRunStatus;

use crate::engine::ConfiguredHandler;
use crate::engine::command_runner::CommandRunResult;
use crate::engine::dispatcher;
use crate::engine::output_parser::UniversalOutput;

/// The shared result of classifying a hook's command output. `before_turn` and
/// `session_start` parse different wire types but turn them into the exact same
/// set of fields, so the classification logic lives here once.
pub(crate) struct UniversalCompletion {
    pub entries: Vec<HookOutputEntry>,
    pub status: HookRunStatus,
    pub should_stop: bool,
    pub stop_reason: Option<String>,
    pub additional_context_for_model: Option<String>,
}

/// Classifies a finished hook run into entries/status/stop signals.
///
/// `parse` maps the hook's stdout into its `(universal, additional_context)`
/// pair (each event kind owns a differently-typed parser); `invalid_json_message`
/// is the error surfaced when stdout looks like JSON but the parser rejects it.
pub(crate) fn classify_universal_completion(
    run_result: &CommandRunResult,
    invalid_json_message: &str,
    parse: impl Fn(&str) -> Option<(UniversalOutput, Option<String>)>,
) -> UniversalCompletion {
    let mut entries = Vec::new();
    let mut status = HookRunStatus::Completed;
    let mut should_stop = false;
    let mut stop_reason = None;
    let mut additional_context_for_model = None;

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
                } else if let Some((universal, additional_context)) = parse(&run_result.stdout) {
                    if let Some(system_message) = universal.system_message {
                        entries.push(HookOutputEntry {
                            kind: HookOutputEntryKind::Warning,
                            text: system_message,
                        });
                    }
                    if let Some(additional_context) = additional_context {
                        entries.push(HookOutputEntry {
                            kind: HookOutputEntryKind::Context,
                            text: additional_context.clone(),
                        });
                        if universal.continue_processing {
                            additional_context_for_model = Some(additional_context);
                        }
                    }
                    let _ = universal.suppress_output;
                    if !universal.continue_processing {
                        status = HookRunStatus::Stopped;
                        should_stop = true;
                        stop_reason = universal.stop_reason.clone();
                        if let Some(stop_reason_text) = universal.stop_reason {
                            entries.push(HookOutputEntry {
                                kind: HookOutputEntryKind::Stop,
                                text: stop_reason_text,
                            });
                        }
                    }
                // Preserve plain-text context support without treating malformed JSON as context.
                } else if trimmed_stdout.starts_with('{') || trimmed_stdout.starts_with('[') {
                    status = HookRunStatus::Failed;
                    entries.push(HookOutputEntry {
                        kind: HookOutputEntryKind::Error,
                        text: invalid_json_message.to_string(),
                    });
                } else {
                    let additional_context = trimmed_stdout.to_string();
                    entries.push(HookOutputEntry {
                        kind: HookOutputEntryKind::Context,
                        text: additional_context.clone(),
                    });
                    additional_context_for_model = Some(additional_context);
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

    UniversalCompletion {
        entries,
        status,
        should_stop,
        stop_reason,
        additional_context_for_model,
    }
}

/// Builds one failed `HookCompletedEvent` per handler, all carrying the same
/// error message. Shared by every event kind's serialization-failure path,
/// which only differ in the outcome struct that wraps these events.
pub(crate) fn failed_hook_events(
    handlers: Vec<ConfiguredHandler>,
    turn_id: Option<String>,
    error_message: String,
) -> Vec<HookCompletedEvent> {
    handlers
        .into_iter()
        .map(|handler| {
            let mut run = dispatcher::running_summary(&handler);
            run.status = HookRunStatus::Failed;
            run.completed_at = Some(run.started_at);
            run.duration_ms = Some(0);
            run.entries = vec![HookOutputEntry {
                kind: HookOutputEntryKind::Error,
                text: error_message.clone(),
            }];
            HookCompletedEvent {
                turn_id: turn_id.clone(),
                run,
            }
        })
        .collect()
}
