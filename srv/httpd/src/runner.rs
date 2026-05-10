use std::sync::Arc;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::{EventMsg, Op, Submission};
use chaos_ipc::user_input::UserInput;
use chaos_kern::Process;
use chaos_kern::ProcessTable;
use chaos_kern::config::Config;
use tracing::{info, warn};

use crate::protocol::{TokenUsageEntry, TokenUsageResponse, TriggerRequest};

/// Outcome of a trigger execution.
pub(crate) struct TriggerOutcome {
    pub process_id: ProcessId,
    pub text: String,
    pub is_error: bool,
    pub usage: Option<TokenUsageResponse>,
}

/// A started process and its id, ready for execution.
pub(crate) struct StartedProcess {
    pub process_id: ProcessId,
    pub process: Arc<Process>,
}

/// Start a fresh process from the process table. The caller is responsible
/// for calling `cleanup` when done (including on timeout).
pub(crate) async fn start(
    process_table: &Arc<ProcessTable>,
    config: Config,
) -> Result<StartedProcess, anyhow::Error> {
    let new_process = process_table.start_process(config).await?;
    let (process_id, process, _session_configured) = new_process.into_parts();
    info!(%process_id, "trigger process started");
    Ok(StartedProcess {
        process_id,
        process,
    })
}

/// Execute a trigger request against an already-started process.
///
/// Submits the prompt, drains events until `TurnComplete` or error, and returns
/// the result with accumulated token usage.
pub(crate) async fn execute(
    started: &StartedProcess,
    request: &TriggerRequest,
    conversation_id: &str,
) -> Result<TriggerOutcome, anyhow::Error> {
    let prompt = request.request.clone().unwrap_or_default();
    let process_id = started.process_id;
    let process = &started.process;

    let op = Op::UserInput {
        items: vec![UserInput::Text {
            text: prompt,
            text_elements: Vec::new(),
        }],
        final_output_json_schema: None,
    };
    process
        .submit_with_id(Submission {
            id: conversation_id.to_string(),
            op,
            trace: None,
        })
        .await?;

    // Drain events.
    let mut usage: Option<TokenUsageResponse> = None;

    let outcome = loop {
        match process.next_event().await {
            Ok(event) => match event.msg {
                EventMsg::TurnComplete(ev) => {
                    break TriggerOutcome {
                        process_id,
                        text: ev.last_agent_message.unwrap_or_default(),
                        is_error: false,
                        usage,
                    };
                }
                EventMsg::Error(ev) => {
                    break TriggerOutcome {
                        process_id,
                        text: ev.message,
                        is_error: true,
                        usage,
                    };
                }
                EventMsg::TokenCount(ev) => {
                    if let Some(info) = ev.info {
                        usage = Some(TokenUsageResponse {
                            total_token_usage: TokenUsageEntry {
                                input_tokens: info.total_token_usage.input_tokens,
                                cached_input_tokens: info.total_token_usage.cached_input_tokens,
                                output_tokens: info.total_token_usage.output_tokens,
                                reasoning_output_tokens: info
                                    .total_token_usage
                                    .reasoning_output_tokens,
                                total_tokens: info.total_token_usage.total_tokens,
                            },
                            last_token_usage: TokenUsageEntry {
                                input_tokens: info.last_token_usage.input_tokens,
                                cached_input_tokens: info.last_token_usage.cached_input_tokens,
                                output_tokens: info.last_token_usage.output_tokens,
                                reasoning_output_tokens: info
                                    .last_token_usage
                                    .reasoning_output_tokens,
                                total_tokens: info.last_token_usage.total_tokens,
                            },
                            model_context_window: info.model_context_window,
                        });
                    }
                }
                EventMsg::TurnAborted(_) | EventMsg::ShutdownComplete => {
                    break TriggerOutcome {
                        process_id,
                        text: "process terminated unexpectedly".to_string(),
                        is_error: true,
                        usage,
                    };
                }
                // Interactive-only events should not appear in headless mode.
                // Log a warning and continue draining.
                EventMsg::ExecApprovalRequest(_)
                | EventMsg::ApplyPatchApprovalRequest(_)
                | EventMsg::ElicitationRequest(_)
                | EventMsg::RequestUserInput(_)
                | EventMsg::RequestPermissions(_)
                | EventMsg::DynamicToolCallRequest(_) => {
                    warn!(
                        %process_id,
                        event = ?std::mem::discriminant(&event.msg),
                        "unexpected interactive event in headless mode, continuing",
                    );
                }
                // All other events (AgentMessage, Warning, ProcessNameUpdated, etc.)
                // are passthrough — no action needed.
                _ => {}
            },
            Err(e) => {
                break TriggerOutcome {
                    process_id,
                    text: format!("runtime error: {e}"),
                    is_error: true,
                    usage,
                };
            }
        }
    };

    Ok(outcome)
}

/// Shut down and remove a process from the process table. The shutdown is
/// bounded by `grace` so a stalled process never blocks the caller
/// indefinitely. `remove_process` is always called regardless of shutdown
/// outcome.
pub(crate) async fn cleanup(
    process_table: &Arc<ProcessTable>,
    started: &StartedProcess,
    grace: std::time::Duration,
) {
    match tokio::time::timeout(grace, started.process.shutdown_and_wait()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            warn!(
                process_id = %started.process_id,
                error = %e,
                "failed to shut down trigger process",
            );
        }
        Err(_) => {
            warn!(
                process_id = %started.process_id,
                grace_secs = grace.as_secs(),
                "trigger process shutdown timed out, removing from table",
            );
        }
    }
    // Always remove from the table so we don't leak tracked processes.
    process_table.remove_process(&started.process_id).await;
}
