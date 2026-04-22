//! Renderer-agnostic frontend reduction of kernel events.

use std::collections::HashMap;
use std::path::PathBuf;

use chaos_ipc::protocol::ChaosErrorInfo;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::TokenUsage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionStatus {
    #[default]
    Booting,
    Ready,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TurnStatus {
    #[default]
    Idle,
    InFlight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptEntry {
    User {
        text: String,
    },
    Agent {
        content: String,
    },
    Reasoning {
        content: String,
    },
    Exec {
        command: Vec<String>,
        cwd: PathBuf,
        exit_code: Option<i32>,
        output: String,
    },
    Tool {
        server: String,
        tool: String,
        result: Option<Result<String, String>>,
    },
    Notice {
        level: NoticeLevel,
        text: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct FrontendState {
    pub transcript: Vec<TranscriptEntry>,
    pub status: SessionStatus,
    pub turn: TurnStatus,
    pub token_usage: Option<TokenUsage>,
    pending_streams: HashMap<String, usize>,
    pending_calls: HashMap<String, usize>,
}

impl FrontendState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn can_submit(&self) -> bool {
        self.status == SessionStatus::Ready && self.turn == TurnStatus::Idle
    }

    pub fn pending_stream_count(&self) -> usize {
        self.pending_streams.len()
    }

    pub fn pending_call_count(&self) -> usize {
        self.pending_calls.len()
    }

    pub fn record_user_submission(&mut self, text: String) {
        self.transcript.push(TranscriptEntry::User { text });
        self.turn = TurnStatus::InFlight;
    }

    pub fn mark_kernel_gone(&mut self) {
        self.status = SessionStatus::Shutdown;
        self.turn = TurnStatus::Idle;
        self.clear_pending_bookkeeping();
        self.transcript.push(TranscriptEntry::Notice {
            level: NoticeLevel::Error,
            text: "op_tx closed — kernel is gone".to_string(),
        });
    }

    pub fn apply_event(&mut self, event: Event) {
        self.apply_event_msg(event.msg);
    }

    pub fn apply_event_msg(&mut self, msg: EventMsg) {
        match msg {
            EventMsg::SessionConfigured(_) => {
                self.status = SessionStatus::Ready;
            }
            EventMsg::ShutdownComplete => {
                self.status = SessionStatus::Shutdown;
                self.turn = TurnStatus::Idle;
                self.clear_pending_bookkeeping();
            }
            EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_) => {
                self.turn = TurnStatus::Idle;
                self.clear_pending_bookkeeping();
            }
            EventMsg::AgentMessageContentDelta(delta) => {
                self.push_stream_entry(StreamKind::Agent, delta.item_id, &delta.delta);
            }
            EventMsg::AgentMessage(msg) => {
                self.finalize_stream_entry(StreamKind::Agent, &msg.message);
            }
            EventMsg::ReasoningContentDelta(delta) => {
                self.push_stream_entry(StreamKind::Reasoning, delta.item_id, &delta.delta);
            }
            EventMsg::AgentReasoning(reasoning) => {
                self.finalize_stream_entry(StreamKind::Reasoning, &reasoning.text);
            }
            EventMsg::ExecCommandBegin(begin) => {
                let idx = self.transcript.len();
                self.transcript.push(TranscriptEntry::Exec {
                    command: begin.command,
                    cwd: begin.cwd,
                    exit_code: None,
                    output: String::new(),
                });
                self.pending_calls.insert(begin.call_id, idx);
            }
            EventMsg::ExecCommandEnd(end) => {
                let preview = if !end.aggregated_output.is_empty() {
                    end.aggregated_output
                } else if !end.stdout.is_empty() {
                    end.stdout
                } else {
                    end.stderr
                };
                if let Some(idx) = self.pending_calls.remove(&end.call_id)
                    && let Some(TranscriptEntry::Exec {
                        exit_code, output, ..
                    }) = self.transcript.get_mut(idx)
                {
                    *exit_code = Some(end.exit_code);
                    *output = preview;
                } else {
                    self.transcript.push(TranscriptEntry::Exec {
                        command: end.command,
                        cwd: end.cwd,
                        exit_code: Some(end.exit_code),
                        output: preview,
                    });
                }
            }
            EventMsg::McpToolCallBegin(begin) => {
                let idx = self.transcript.len();
                self.transcript.push(TranscriptEntry::Tool {
                    server: begin.invocation.server,
                    tool: begin.invocation.tool,
                    result: None,
                });
                self.pending_calls.insert(begin.call_id, idx);
            }
            EventMsg::McpToolCallEnd(end) => {
                let outcome = match &end.result {
                    Ok(_) => Ok(format!("ok in {:?}", end.duration)),
                    Err(err) => Err(err.clone()),
                };
                if let Some(idx) = self.pending_calls.remove(&end.call_id)
                    && let Some(TranscriptEntry::Tool { result, .. }) = self.transcript.get_mut(idx)
                {
                    *result = Some(outcome);
                } else {
                    self.transcript.push(TranscriptEntry::Tool {
                        server: end.invocation.server,
                        tool: end.invocation.tool,
                        result: Some(outcome),
                    });
                }
            }
            EventMsg::Error(err) => {
                self.transcript.push(TranscriptEntry::Notice {
                    level: NoticeLevel::Error,
                    text: format_error(&err.message, err.chaos_error_info.as_ref()),
                });
                self.turn = TurnStatus::Idle;
                self.clear_pending_bookkeeping();
            }
            EventMsg::StreamError(err) => {
                self.transcript.push(TranscriptEntry::Notice {
                    level: NoticeLevel::Warn,
                    text: format!(
                        "stream hiccup: {}",
                        format_error(&err.message, err.chaos_error_info.as_ref())
                    ),
                });
            }
            EventMsg::Warning(warn) => {
                self.transcript.push(TranscriptEntry::Notice {
                    level: NoticeLevel::Warn,
                    text: warn.message,
                });
            }
            EventMsg::BackgroundEvent(bg) => {
                self.transcript.push(TranscriptEntry::Notice {
                    level: NoticeLevel::Info,
                    text: bg.message,
                });
            }
            EventMsg::DeprecationNotice(notice) => {
                self.transcript.push(TranscriptEntry::Notice {
                    level: NoticeLevel::Warn,
                    text: format!("deprecated: {notice:?}"),
                });
            }
            EventMsg::TokenCount(tc) => {
                if let Some(info) = tc.info {
                    self.token_usage = Some(info.total_token_usage);
                }
            }
            _ => {}
        }
    }

    fn clear_pending_bookkeeping(&mut self) {
        self.pending_streams.clear();
        self.pending_calls.clear();
    }

    fn push_stream_entry(&mut self, kind: StreamKind, item_id: String, delta: &str) {
        if let Some(idx) = self.pending_streams.get(&item_id).copied() {
            match (kind, self.transcript.get_mut(idx)) {
                (StreamKind::Agent, Some(TranscriptEntry::Agent { content }))
                | (StreamKind::Reasoning, Some(TranscriptEntry::Reasoning { content })) => {
                    content.push_str(delta);
                    return;
                }
                _ => {
                    self.pending_streams.remove(&item_id);
                }
            }
        }

        let idx = self.transcript.len();
        let entry = match kind {
            StreamKind::Agent => TranscriptEntry::Agent {
                content: delta.to_string(),
            },
            StreamKind::Reasoning => TranscriptEntry::Reasoning {
                content: delta.to_string(),
            },
        };
        self.transcript.push(entry);
        self.pending_streams.insert(item_id, idx);
    }

    fn finalize_stream_entry(&mut self, kind: StreamKind, full: &str) {
        for idx in (0..self.transcript.len()).rev() {
            match &self.transcript[idx] {
                TranscriptEntry::User { .. } => break,
                TranscriptEntry::Agent { .. } if kind == StreamKind::Agent => {
                    self.transcript[idx] = TranscriptEntry::Agent {
                        content: full.to_string(),
                    };
                    self.pending_streams.retain(|_, value| *value != idx);
                    return;
                }
                TranscriptEntry::Reasoning { .. } if kind == StreamKind::Reasoning => {
                    self.transcript[idx] = TranscriptEntry::Reasoning {
                        content: full.to_string(),
                    };
                    self.pending_streams.retain(|_, value| *value != idx);
                    return;
                }
                _ => {}
            }
        }

        self.transcript.push(match kind {
            StreamKind::Agent => TranscriptEntry::Agent {
                content: full.to_string(),
            },
            StreamKind::Reasoning => TranscriptEntry::Reasoning {
                content: full.to_string(),
            },
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Agent,
    Reasoning,
}

/// Render an `ErrorEvent`-style message + optional structured tag into a
/// human-readable single line.
pub fn format_error(message: &str, info: Option<&ChaosErrorInfo>) -> String {
    let tag: &str = match info {
        None => return message.to_string(),
        Some(ChaosErrorInfo::ContextWindowExceeded) => "context window exceeded",
        Some(ChaosErrorInfo::UsageLimitExceeded) => "usage limit reached",
        Some(ChaosErrorInfo::ServerOverloaded) => "server overloaded",
        Some(ChaosErrorInfo::Unauthorized) => "unauthorized",
        Some(ChaosErrorInfo::ProviderAuthMissing { .. }) => "missing provider credentials",
        Some(ChaosErrorInfo::BadRequest) => "bad request",
        Some(ChaosErrorInfo::SandboxError) => "sandbox error",
        Some(ChaosErrorInfo::InternalServerError) => "internal server error",
        Some(ChaosErrorInfo::ProcessRollbackFailed) => "process rollback failed",
        Some(ChaosErrorInfo::Other) => "error",
        Some(ChaosErrorInfo::HttpConnectionFailed { http_status_code }) => {
            return format_http_error("http connection failed", *http_status_code, message);
        }
        Some(ChaosErrorInfo::ResponseStreamConnectionFailed { http_status_code }) => {
            return format_http_error("response stream connect failed", *http_status_code, message);
        }
        Some(ChaosErrorInfo::ResponseStreamDisconnected { http_status_code }) => {
            return format_http_error("response stream disconnected", *http_status_code, message);
        }
        Some(ChaosErrorInfo::ResponseTooManyFailedAttempts { http_status_code }) => {
            return format_http_error("retries exhausted", *http_status_code, message);
        }
    };
    format!("{tag} — {message}")
}

fn format_http_error(tag: &str, status: Option<u16>, message: &str) -> String {
    match status {
        Some(code) => format!("{tag} (http {code}) — {message}"),
        None => format!("{tag} — {message}"),
    }
}

#[cfg(test)]
mod tests {
    use chaos_ipc::ProcessId;
    use chaos_ipc::config_types::ApprovalsReviewer;
    use chaos_ipc::protocol::AgentMessageContentDeltaEvent;
    use chaos_ipc::protocol::AgentMessageEvent;
    use chaos_ipc::protocol::ApprovalPolicy;
    use chaos_ipc::protocol::BackgroundEventEvent;
    use chaos_ipc::protocol::ErrorEvent;
    use chaos_ipc::protocol::ExecCommandEndEvent;
    use chaos_ipc::protocol::ExecCommandSource;
    use chaos_ipc::protocol::ExecCommandStatus;
    use chaos_ipc::protocol::SandboxPolicy;
    use chaos_ipc::protocol::SessionConfiguredEvent;
    use chaos_ipc::protocol::TokenCountEvent;
    use chaos_ipc::protocol::TokenUsage;
    use chaos_ipc::protocol::TokenUsageInfo;
    use chaos_ipc::protocol::TurnCompleteEvent;
    use pretty_assertions::assert_eq;

    use super::*;

    fn session_configured() -> SessionConfiguredEvent {
        SessionConfiguredEvent {
            session_id: ProcessId::default(),
            forked_from_id: None,
            process_name: None,
            model: String::new(),
            model_provider_id: "chassis-test".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::default(),
            approvals_reviewer: ApprovalsReviewer::default(),
            vfs_policy: chaos_ipc::protocol::VfsPolicy::from(&SandboxPolicy::new_read_only_policy()),
            socket_policy: chaos_ipc::protocol::SocketPolicy::from(
                &SandboxPolicy::new_read_only_policy(),
            ),
            cwd: PathBuf::from("/"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }
    }

    #[test]
    fn full_turn_lifecycle_reduces_to_transcript_state() {
        let mut state = FrontendState::new();
        state.apply_event_msg(EventMsg::SessionConfigured(session_configured()));
        assert_eq!(SessionStatus::Ready, state.status);

        state.record_user_submission("hello".to_string());
        assert_eq!(TurnStatus::InFlight, state.turn);
        assert!(matches!(
            state.transcript.last(),
            Some(TranscriptEntry::User { text }) if text == "hello"
        ));

        state.apply_event_msg(EventMsg::AgentMessageContentDelta(
            AgentMessageContentDeltaEvent {
                process_id: "p".to_string(),
                turn_id: "t".to_string(),
                item_id: "item-1".to_string(),
                delta: "hi".to_string(),
            },
        ));
        assert!(matches!(
            state.transcript.last(),
            Some(TranscriptEntry::Agent { content }) if content == "hi"
        ));

        state.apply_event_msg(EventMsg::AgentMessage(AgentMessageEvent {
            message: "hi there".to_string(),
            phase: None,
        }));
        assert!(matches!(
            state.transcript.last(),
            Some(TranscriptEntry::Agent { content }) if content == "hi there"
        ));

        state.apply_event_msg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "t1".to_string(),
            last_agent_message: Some("hi there".to_string()),
        }));
        assert_eq!(TurnStatus::Idle, state.turn);
        assert_eq!(0, state.pending_stream_count());
        assert_eq!(0, state.pending_call_count());
    }

    #[test]
    fn errors_and_token_usage_update_state() {
        let mut state = FrontendState::new();
        state.record_user_submission("hello".to_string());

        state.apply_event_msg(EventMsg::TokenCount(TokenCountEvent {
            info: Some(TokenUsageInfo {
                total_token_usage: TokenUsage {
                    input_tokens: 1,
                    cached_input_tokens: 0,
                    output_tokens: 2,
                    reasoning_output_tokens: 0,
                    total_tokens: 3,
                },
                last_token_usage: TokenUsage::default(),
                model_context_window: None,
            }),
            rate_limits: None,
        }));
        assert_eq!(
            Some(3),
            state.token_usage.as_ref().map(|usage| usage.total_tokens)
        );

        state.apply_event_msg(EventMsg::BackgroundEvent(BackgroundEventEvent {
            message: "queued".to_string(),
        }));
        state.apply_event_msg(EventMsg::Error(ErrorEvent {
            message: "boom".to_string(),
            chaos_error_info: Some(ChaosErrorInfo::ContextWindowExceeded),
        }));

        assert_eq!(TurnStatus::Idle, state.turn);
        assert!(matches!(
            state.transcript.last(),
            Some(TranscriptEntry::Notice {
                level: NoticeLevel::Error,
                text,
            }) if text.contains("context window exceeded") && text.contains("boom")
        ));
    }

    #[test]
    fn orphan_exec_end_still_renders() {
        let mut state = FrontendState::new();
        state.apply_event_msg(EventMsg::ExecCommandEnd(ExecCommandEndEvent {
            call_id: "call-1".to_string(),
            command: vec!["echo".to_string(), "hi".to_string()],
            aggregated_output: "hi".to_string(),
            parsed_cmd: Vec::new(),
            cwd: PathBuf::from("/tmp"),
            exit_code: 0,
            stdout: "hi".to_string(),
            stderr: String::new(),
            duration: std::time::Duration::from_secs(1),
            formatted_output: String::new(),
            status: ExecCommandStatus::Completed,
            source: ExecCommandSource::Agent,
            process_id: None,
            turn_id: "t".to_string(),
            interaction_input: None,
        }));

        assert!(matches!(
            state.transcript.last(),
            Some(TranscriptEntry::Exec {
                exit_code: Some(0),
                output,
                ..
            }) if output == "hi"
        ));
    }
}
