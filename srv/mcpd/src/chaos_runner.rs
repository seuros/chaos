//! Unified Chaos session runner — handles both new and resumed processes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as RequestMutex};

use crate::elicitation::handle_mcp_server_elicitation_complete;
use crate::elicitation::handle_mcp_server_elicitation_request;
use crate::exec_approval::handle_exec_approval_request;
use crate::outgoing_message::OutgoingMessageSender;
use crate::outgoing_message::OutgoingNotificationMeta;
use crate::patch_approval::handle_patch_approval_request;
use chaos_ipc::ProcessId;
use chaos_ipc::product::OS_NAME;
use chaos_ipc::protocol::AgentMessageEvent;
use chaos_ipc::protocol::ApplyPatchApprovalRequestEvent;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ExecApprovalRequestEvent;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::Submission;
use chaos_ipc::protocol::TurnCompleteEvent;
use chaos_ipc::user_input::UserInput;
use chaos_kern::Process;
use chaos_kern::ProcessTable;
use chaos_kern::config::Config as ChaosConfig;
use chaos_session::background::{BackgroundWait, WaitEvent};
use mcp_host::managers::progress::Progress;
use mcp_host::protocol::types::RequestId;
use tokio::sync::Mutex;

/// Outcome of a Chaos session run.
pub(crate) struct SessionOutcome {
    pub process_id: ProcessId,
    pub text: String,
    pub is_error: bool,
}

/// Shared cache for process names observed from ProcessNameUpdated events.
pub(crate) type ProcessNameCache = Arc<Mutex<HashMap<ProcessId, String>>>;

pub(crate) struct RunChaosSessionArgs {
    pub request_id: RequestId,
    pub prompt: String,
    pub config: Option<ChaosConfig>,
    pub existing_process_id: Option<ProcessId>,
    pub outgoing: Arc<OutgoingMessageSender>,
    pub process_table: Arc<ProcessTable>,
    pub running_requests: Arc<RequestMutex<HashMap<RequestId, ProcessId>>>,
    pub process_names: ProcessNameCache,
    pub progress: Option<Progress>,
    pub wait_background: bool,
    pub background_timeout: std::time::Duration,
}

/// Resolved process — either newly created or resumed from an existing ID.
struct ResolvedProcess {
    process_id: ProcessId,
    process: Arc<Process>,
}

/// Request cancellation drops its future. Remove its ID synchronously and
/// interrupt only the process bound to that originating request.
struct ActiveRequest {
    id: RequestId,
    running: Arc<RequestMutex<HashMap<RequestId, ProcessId>>>,
    process: Arc<Process>,
    complete: bool,
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
        if !self.complete
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let process = self.process.clone();
            runtime.spawn(async move {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_millis(250),
                    process.submit(Op::Interrupt),
                )
                .await;
            });
        }
    }
}

/// Unified entry point: create or resume a Chaos session.
///
/// Returns a `SessionOutcome` — the caller (tool handler) converts this
/// to the appropriate `ToolOutput`. Notifications are streamed via `outgoing`.
/// If request-scoped `progress` is provided, MCP progress notifications are sent at
/// key milestones so the client can display status.
pub(crate) async fn run_chaos_session(args: RunChaosSessionArgs) -> SessionOutcome {
    let RunChaosSessionArgs {
        request_id,
        prompt,
        config,
        existing_process_id,
        outgoing,
        process_table,
        running_requests,
        process_names,
        progress,
        wait_background,
        background_timeout,
    } = args;

    // Send progress if the client requested it.
    if let Some(ref p) = progress {
        p.report(0.0, Some(4.0), Some("Resolving process...".into()));
    }

    // Phase 1: resolve process
    let resolved = match existing_process_id {
        Some(pid) => match process_table.get_process(pid).await {
            Ok(process) => ResolvedProcess {
                process_id: pid,
                process,
            },
            Err(e) => {
                return SessionOutcome {
                    process_id: pid,
                    text: format!("Session not found for process_id {pid}: {e}"),
                    is_error: true,
                };
            }
        },
        None => {
            let Some(config) = config else {
                return SessionOutcome {
                    process_id: ProcessId::new(),
                    text: "config required for new processes".to_string(),
                    is_error: true,
                };
            };
            match process_table.start_process(config).await {
                Ok(new_process) => {
                    let (process_id, process, session_configured) = new_process.into_parts();
                    let event = Event {
                        id: String::new(),
                        msg: EventMsg::SessionConfigured(session_configured),
                    };
                    outgoing
                        .send_event_as_notification(
                            &event,
                            Some(OutgoingNotificationMeta {
                                request_id: Some(request_id.clone()),
                                process_id: Some(process_id),
                            }),
                        )
                        .await;
                    ResolvedProcess {
                        process_id,
                        process,
                    }
                }
                Err(e) => {
                    return SessionOutcome {
                        process_id: ProcessId::new(),
                        text: format!("Failed to start {OS_NAME} session: {e}"),
                        is_error: true,
                    };
                }
            }
        }
    };

    let ResolvedProcess {
        process_id,
        process,
    } = resolved;

    if let Some(ref p) = progress {
        p.report(1.0, Some(4.0), Some("Configuring session...".into()));
    }

    // Phase 2: submit prompt
    running_requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(request_id.clone(), process_id);
    let mut request = ActiveRequest {
        id: request_id.clone(),
        running: running_requests.clone(),
        process: process.clone(),
        complete: false,
    };

    let user_input = process
        .user_turn(
            vec![UserInput::Text {
                text: prompt,
                text_elements: Vec::new(),
            }],
            None,
        )
        .await;

    let submit_err = if existing_process_id.is_some() {
        process.submit(user_input).await.err()
    } else {
        process
            .submit_with_id(Submission {
                id: request_id.to_string(),
                op: user_input,
                trace: None,
            })
            .await
            .err()
    };

    if let Some(e) = submit_err {
        tracing::error!("Failed to submit prompt: {e}");
        return SessionOutcome {
            process_id,
            text: format!("Failed to submit prompt: {e}"),
            is_error: true,
        };
    }

    if let Some(ref p) = progress {
        p.report(2.0, Some(4.0), Some("Streaming response...".into()));
    }

    // Phase 3: event loop
    let wait = BackgroundWait::new(&process, wait_background, background_timeout);
    let outcome = run_event_loop(
        process_id,
        process,
        outgoing,
        request_id,
        process_names,
        wait,
    )
    .await;
    request.complete = true;

    if let Some(ref p) = progress {
        let msg = if outcome.is_error {
            "Failed"
        } else {
            "Complete"
        };
        p.report(4.0, Some(4.0), Some(msg.into()));
    }

    outcome
}

/// Stream Chaos events until TurnComplete or error.
async fn run_event_loop(
    process_id: ProcessId,
    process: Arc<Process>,
    outgoing: Arc<OutgoingMessageSender>,
    request_id: RequestId,
    process_names: ProcessNameCache,
    mut wait: BackgroundWait,
) -> SessionOutcome {
    let request_id_str = request_id.to_string();
    let wait_background = wait.is_enabled();
    let mut text = String::new();

    loop {
        match wait.next(&process).await {
            WaitEvent::Complete => {
                return SessionOutcome {
                    process_id,
                    text,
                    is_error: false,
                };
            }
            WaitEvent::Event(event) => {
                outgoing
                    .send_event_as_notification(
                        &event,
                        Some(OutgoingNotificationMeta {
                            request_id: Some(request_id.clone()),
                            process_id: Some(process_id),
                        }),
                    )
                    .await;

                match event.msg {
                    EventMsg::ExecApprovalRequest(ev) => {
                        let approval_id = ev.effective_approval_id();
                        let ExecApprovalRequestEvent {
                            turn_id: _,
                            command,
                            cwd,
                            call_id,
                            approval_id: _,
                            reason: _,
                            proposed_execpolicy_amendment: _,
                            proposed_network_policy_amendments: _,
                            parsed_cmd,
                            network_approval_context: _,
                            additional_permissions: _,
                            available_decisions: _,
                        } = ev;
                        handle_exec_approval_request(
                            command,
                            cwd,
                            outgoing.clone(),
                            process.clone(),
                            request_id.clone(),
                            request_id_str.clone(),
                            event.id.clone(),
                            call_id,
                            approval_id,
                            parsed_cmd,
                            process_id,
                        )
                        .await;
                    }
                    EventMsg::Error(err_event) => {
                        return SessionOutcome {
                            process_id,
                            text: err_event.message,
                            is_error: true,
                        };
                    }
                    EventMsg::ElicitationRequest(request) => {
                        handle_mcp_server_elicitation_request(
                            request,
                            outgoing.clone(),
                            process.clone(),
                            process_id,
                        )
                        .await;
                    }
                    EventMsg::ElicitationComplete(ev) => {
                        handle_mcp_server_elicitation_complete(
                            process_id,
                            ev.server_name,
                            ev.elicitation_id,
                            outgoing.clone(),
                        )
                        .await;
                    }
                    EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
                        call_id,
                        turn_id: _,
                        reason,
                        grant_root,
                        changes,
                    }) => {
                        handle_patch_approval_request(
                            call_id,
                            reason,
                            grant_root,
                            changes,
                            outgoing.clone(),
                            process.clone(),
                            request_id.clone(),
                            request_id_str.clone(),
                            event.id.clone(),
                            process_id,
                        )
                        .await;
                    }
                    EventMsg::TurnComplete(TurnCompleteEvent {
                        last_agent_message, ..
                    }) => {
                        if wait_background {
                            text = last_agent_message.unwrap_or_default();
                            continue;
                        }
                        // A persistent MCP session no longer has a waiting
                        // caller; retain results without unattended sampling.
                        let _ = process.submit(Op::Interrupt).await;
                        return SessionOutcome {
                            process_id,
                            text: last_agent_message.unwrap_or_default(),
                            is_error: false,
                        };
                    }
                    EventMsg::ProcessNameUpdated(ev) => {
                        if let Some(name) = ev.process_name {
                            process_names.lock().await.insert(process_id, name);
                        }
                    }
                    // Events forwarded as notifications — no special handling.
                    EventMsg::PlanDelta(_)
                    | EventMsg::Warning(_)
                    | EventMsg::SessionConfigured(_)
                    | EventMsg::McpStartupUpdate(_)
                    | EventMsg::McpStartupComplete(_)
                    | EventMsg::PermissionsUpdated(_)
                    | EventMsg::McpServersRefreshed(_)
                    | EventMsg::AgentMessage(AgentMessageEvent { .. })
                    | EventMsg::AgentReasoningRawContent(_)
                    | EventMsg::TurnStarted(_)
                    | EventMsg::TokenCount(_)
                    | EventMsg::TurnProgress(_)
                    | EventMsg::AgentReasoning(_)
                    | EventMsg::AgentReasoningSectionBreak(_)
                    | EventMsg::McpToolCallBegin(_)
                    | EventMsg::McpToolCallEnd(_)
                    | EventMsg::McpListToolsResponse(_)
                    | EventMsg::AllToolsResponse(_)
                    | EventMsg::ListCustomPromptsResponse(_)
                    | EventMsg::ListModelsResponse(_)
                    | EventMsg::ExecCommandBegin(_)
                    | EventMsg::TerminalInteraction(_)
                    | EventMsg::ExecCommandOutputDelta(_)
                    | EventMsg::ExecCommandEnd(_)
                    | EventMsg::BackgroundEvent(_)
                    | EventMsg::StreamError(_)
                    | EventMsg::PatchApplyBegin(_)
                    | EventMsg::PatchApplyEnd(_)
                    | EventMsg::TurnDiff(_)
                    | EventMsg::WebSearchBegin(_)
                    | EventMsg::WebSearchEnd(_)
                    | EventMsg::GetHistoryEntryResponse(_)
                    | EventMsg::PlanUpdate(_)
                    | EventMsg::TurnAborted(_)
                    | EventMsg::UserMessage(_)
                    | EventMsg::ShutdownComplete
                    | EventMsg::ViewImageToolCall(_)
                    | EventMsg::ImageGenerationBegin(_)
                    | EventMsg::ImageGenerationEnd(_)
                    | EventMsg::RawResponseItem(_)
                    | EventMsg::EnteredReviewMode(_)
                    | EventMsg::ItemStarted(_)
                    | EventMsg::ItemCompleted(_)
                    | EventMsg::HookStarted(_)
                    | EventMsg::HookCompleted(_)
                    | EventMsg::AgentMessageContentDelta(_)
                    | EventMsg::ReasoningContentDelta(_)
                    | EventMsg::ReasoningRawContentDelta(_)
                    | EventMsg::ExitedReviewMode(_)
                    | EventMsg::RequestUserInput(_)
                    | EventMsg::RequestPermissions(_)
                    | EventMsg::DynamicToolCallRequest(_)
                    | EventMsg::DynamicToolCallResponse(_)
                    | EventMsg::ContextCompacted(_)
                    | EventMsg::ModelReroute(_)
                    | EventMsg::ParentEffortChanged(_)
                    | EventMsg::SessionModeChanged(_)
                    | EventMsg::ProcessRolledBack(_)
                    | EventMsg::CollabAgentSpawnBegin(_)
                    | EventMsg::CollabAgentSpawnEnd(_)
                    | EventMsg::CollabAgentInteractionBegin(_)
                    | EventMsg::CollabAgentInteractionEnd(_)
                    | EventMsg::CollabWaitingBegin(_)
                    | EventMsg::CollabWaitingEnd(_)
                    | EventMsg::CollabCloseBegin(_)
                    | EventMsg::CollabCloseEnd(_)
                    | EventMsg::CollabResumeBegin(_)
                    | EventMsg::CollabResumeEnd(_)
                    | EventMsg::CollabAgentStatusChanged(_)
                    | EventMsg::DeprecationNotice(_) => {
                        // Already forwarded as notification above.
                    }
                }
            }
            WaitEvent::Stopped(e) => {
                let _ = process.submit(Op::Interrupt).await;
                return SessionOutcome {
                    process_id,
                    text: format!("{OS_NAME} runtime error: {e}"),
                    is_error: true,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests;
