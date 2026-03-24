//! Unified Chaos session runner — handles both new and resumed threads.

use std::collections::HashMap;
use std::sync::Arc;

use crate::elicitation::handle_mcp_server_elicitation_complete;
use crate::elicitation::handle_mcp_server_elicitation_request;
use crate::exec_approval::handle_exec_approval_request;
use crate::outgoing_message::OutgoingMessageSender;
use crate::outgoing_message::OutgoingNotificationMeta;
use crate::patch_approval::handle_patch_approval_request;
use codex_core::CodexThread;
use codex_core::NewThread;
use codex_core::ThreadManager;
use codex_core::config::Config as CodexConfig;
use codex_protocol::ThreadId;
use codex_protocol::protocol::AgentMessageEvent;
use codex_protocol::protocol::ApplyPatchApprovalRequestEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecApprovalRequestEvent;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::Submission;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::user_input::UserInput;
use mcp_host::protocol::types::RequestId;
use tokio::sync::Mutex;

/// Lightweight MCP progress sender — sends `notifications/progress` via
/// the existing `OutgoingMessageSender` channel.
struct ProgressSender {
    token: String,
    outgoing: Arc<OutgoingMessageSender>,
}

impl ProgressSender {
    fn new(token: String, outgoing: Arc<OutgoingMessageSender>) -> Self {
        Self { token, outgoing }
    }

    async fn send(&self, progress: u32, total: u32, message: &str) {
        let notification = crate::outgoing_message::OutgoingNotification {
            method: "notifications/progress".to_string(),
            params: Some(serde_json::json!({
                "progressToken": self.token,
                "progress": progress,
                "total": total,
                "message": message,
            })),
        };
        self.outgoing.send_notification(notification).await;
    }
}

/// Outcome of a Chaos session run.
pub(crate) struct SessionOutcome {
    pub thread_id: ThreadId,
    pub text: String,
    pub is_error: bool,
}

/// Shared cache for thread names observed from ThreadNameUpdated events.
pub(crate) type ThreadNameCache = Arc<Mutex<HashMap<ThreadId, String>>>;

/// Resolved thread — either newly created or resumed from an existing ID.
struct ResolvedThread {
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
}

/// Unified entry point: create or resume a Chaos session.
///
/// Returns a `SessionOutcome` — the caller (tool handler) converts this
/// to the appropriate `ToolOutput`. Notifications are streamed via `outgoing`.
/// If `progress_token` is provided, MCP progress notifications are sent at
/// key milestones so the client can display status.
pub(crate) async fn run_chaos_session(
    request_id: RequestId,
    prompt: String,
    config: Option<CodexConfig>,
    existing_thread_id: Option<ThreadId>,
    outgoing: Arc<OutgoingMessageSender>,
    thread_manager: Arc<ThreadManager>,
    running_requests: Arc<Mutex<HashMap<RequestId, ThreadId>>>,
    thread_names: ThreadNameCache,
    progress_token: Option<String>,
) -> SessionOutcome {
    // Send progress if the client requested it.
    let progress = progress_token.as_ref().map(|token| {
        ProgressSender::new(token.clone(), outgoing.clone())
    });
    if let Some(ref p) = progress {
        p.send(0, 4, "Resolving thread...").await;
    }

    // Phase 1: resolve thread
    let resolved = match existing_thread_id {
        Some(tid) => {
            match thread_manager.get_thread(tid).await {
                Ok(thread) => ResolvedThread {
                    thread_id: tid,
                    thread,
                },
                Err(e) => {
                    return SessionOutcome {
                        thread_id: tid,
                        text: format!("Session not found for thread_id {tid}: {e}"),
                        is_error: true,
                    };
                }
            }
        }
        None => {
            let config = config.expect("config required for new threads");
            match thread_manager.start_thread(config).await {
                Ok(NewThread {
                    thread_id,
                    thread,
                    session_configured,
                }) => {
                    let event = Event {
                        id: String::new(),
                        msg: EventMsg::SessionConfigured(session_configured),
                    };
                    outgoing
                        .send_event_as_notification(
                            &event,
                            Some(OutgoingNotificationMeta {
                                request_id: Some(request_id.clone()),
                                thread_id: Some(thread_id),
                            }),
                        )
                        .await;
                    ResolvedThread { thread_id, thread }
                }
                Err(e) => {
                    return SessionOutcome {
                        thread_id: ThreadId::new(),
                        text: format!("Failed to start Codex session: {e}"),
                        is_error: true,
                    };
                }
            }
        }
    };

    let ResolvedThread { thread_id, thread } = resolved;

    if let Some(ref p) = progress {
        p.send(1, 4, "Configuring session...").await;
    }

    // Phase 2: submit prompt
    running_requests
        .lock()
        .await
        .insert(request_id.clone(), thread_id);

    let user_input = Op::UserInput {
        items: vec![UserInput::Text {
            text: prompt,
            text_elements: Vec::new(),
        }],
        final_output_json_schema: None,
    };

    let submit_err = if existing_thread_id.is_some() {
        thread.submit(user_input).await.err()
    } else {
        thread
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
        running_requests.lock().await.remove(&request_id);
        return SessionOutcome {
            thread_id,
            text: format!("Failed to submit prompt: {e}"),
            is_error: true,
        };
    }

    if let Some(ref p) = progress {
        p.send(2, 4, "Streaming response...").await;
    }

    // Phase 3: event loop
    let outcome = run_event_loop(thread_id, thread, outgoing, request_id, running_requests, thread_names).await;

    if let Some(ref p) = progress {
        let msg = if outcome.is_error { "Failed" } else { "Complete" };
        p.send(4, 4, msg).await;
    }

    outcome
}

/// Stream Codex events until TurnComplete or error.
async fn run_event_loop(
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    outgoing: Arc<OutgoingMessageSender>,
    request_id: RequestId,
    running_requests: Arc<Mutex<HashMap<RequestId, ThreadId>>>,
    thread_names: ThreadNameCache,
) -> SessionOutcome {
    let request_id_str = request_id.to_string();

    loop {
        match thread.next_event().await {
            Ok(event) => {
                outgoing
                    .send_event_as_notification(
                        &event,
                        Some(OutgoingNotificationMeta {
                            request_id: Some(request_id.clone()),
                            thread_id: Some(thread_id),
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
                            skill_metadata: _,
                            available_decisions: _,
                        } = ev;
                        handle_exec_approval_request(
                            command,
                            cwd,
                            outgoing.clone(),
                            thread.clone(),
                            request_id.clone(),
                            request_id_str.clone(),
                            event.id.clone(),
                            call_id,
                            approval_id,
                            parsed_cmd,
                            thread_id,
                        )
                        .await;
                    }
                    EventMsg::Error(err_event) => {
                        return SessionOutcome {
                            thread_id,
                            text: err_event.message,
                            is_error: true,
                        };
                    }
                    EventMsg::ElicitationRequest(request) => {
                        handle_mcp_server_elicitation_request(
                            request,
                            outgoing.clone(),
                            thread.clone(),
                        )
                        .await;
                    }
                    EventMsg::ElicitationComplete(ev) => {
                        handle_mcp_server_elicitation_complete(
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
                            thread.clone(),
                            request_id.clone(),
                            request_id_str.clone(),
                            event.id.clone(),
                            thread_id,
                        )
                        .await;
                    }
                    EventMsg::TurnComplete(TurnCompleteEvent {
                        last_agent_message, ..
                    }) => {
                        running_requests.lock().await.remove(&request_id);
                        return SessionOutcome {
                            thread_id,
                            text: last_agent_message.unwrap_or_default(),
                            is_error: false,
                        };
                    }
                    EventMsg::ThreadNameUpdated(ev) => {
                        if let Some(name) = ev.thread_name {
                            thread_names.lock().await.insert(thread_id, name);
                        }
                    }
                    // Events forwarded as notifications — no special handling.
                    EventMsg::PlanDelta(_)
                    | EventMsg::Warning(_)
                    | EventMsg::GuardianAssessment(_)
                    | EventMsg::SessionConfigured(_)
                    | EventMsg::AgentMessageDelta(_)
                    | EventMsg::AgentReasoningDelta(_)
                    | EventMsg::McpStartupUpdate(_)
                    | EventMsg::McpStartupComplete(_)
                    | EventMsg::AgentMessage(AgentMessageEvent { .. })
                    | EventMsg::AgentReasoningRawContent(_)
                    | EventMsg::AgentReasoningRawContentDelta(_)
                    | EventMsg::TurnStarted(_)
                    | EventMsg::TokenCount(_)
                    | EventMsg::AgentReasoning(_)
                    | EventMsg::AgentReasoningSectionBreak(_)
                    | EventMsg::McpToolCallBegin(_)
                    | EventMsg::McpToolCallEnd(_)
                    | EventMsg::McpListToolsResponse(_)
                    | EventMsg::ListCustomPromptsResponse(_)
                    | EventMsg::ListSkillsResponse(_)
                    | EventMsg::ListRemoteSkillsResponse(_)
                    | EventMsg::RemoteSkillDownloaded(_)
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
                    | EventMsg::SkillsUpdateAvailable
                    | EventMsg::UndoStarted(_)
                    | EventMsg::UndoCompleted(_)
                    | EventMsg::ExitedReviewMode(_)
                    | EventMsg::RequestUserInput(_)
                    | EventMsg::RequestPermissions(_)
                    | EventMsg::DynamicToolCallRequest(_)
                    | EventMsg::DynamicToolCallResponse(_)
                    | EventMsg::ContextCompacted(_)
                    | EventMsg::ModelReroute(_)
                    | EventMsg::ThreadRolledBack(_)
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
                    | EventMsg::DeprecationNotice(_) => {
                        // Already forwarded as notification above.
                    }
                }
            }
            Err(e) => {
                return SessionOutcome {
                    thread_id,
                    text: format!("Codex runtime error: {e}"),
                    is_error: true,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn session_outcome_captures_thread_id() {
        let thread_id = ThreadId::new();
        let outcome = SessionOutcome {
            thread_id,
            text: "done".to_string(),
            is_error: false,
        };
        assert_eq!(outcome.thread_id, thread_id);
        assert_eq!(outcome.text, "done");
        assert!(!outcome.is_error);
    }
}
