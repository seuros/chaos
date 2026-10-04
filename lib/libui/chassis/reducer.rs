//! Renderer-agnostic frontend reduction of kernel events.

use std::path::PathBuf;

use chaos_ipc::protocol::ChaosErrorInfo;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::TokenUsage;

mod lifecycle;

pub use lifecycle::{SessionStatus, TurnStatus};

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
        server: Option<String>,
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
    pub token_usage: Option<TokenUsage>,
    lifecycle: lifecycle::Lifecycle,
}

impl FrontendState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn can_submit(&self) -> bool {
        self.status() == SessionStatus::Ready && self.turn() == TurnStatus::Idle
    }

    pub fn status(&self) -> SessionStatus {
        self.lifecycle.status()
    }

    pub fn turn(&self) -> TurnStatus {
        self.lifecycle.turn()
    }

    pub fn pending_stream_count(&self) -> usize {
        self.lifecycle.pending().streams.len()
    }

    pub fn pending_call_count(&self) -> usize {
        self.lifecycle.pending().calls.len()
    }

    pub fn record_user_submission(&mut self, text: String) {
        if self.status() == SessionStatus::Shutdown {
            return;
        }
        self.transcript.push(TranscriptEntry::User { text });
        self.lifecycle.submit();
    }

    pub fn mark_kernel_gone(&mut self) {
        self.lifecycle.shutdown();
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
                self.lifecycle.configure();
            }
            EventMsg::ShutdownComplete => {
                self.lifecycle.shutdown();
            }
            EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_) => {
                self.lifecycle.finish();
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
                self.lifecycle
                    .pending_mut()
                    .calls
                    .insert(begin.call_id, idx);
            }
            EventMsg::ExecCommandEnd(end) => {
                let preview = if !end.aggregated_output.is_empty() {
                    end.aggregated_output
                } else if !end.stdout.is_empty() {
                    end.stdout
                } else {
                    end.stderr
                };
                if let Some(idx) = self.lifecycle.pending_mut().calls.remove(&end.call_id)
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
                self.lifecycle
                    .pending_mut()
                    .calls
                    .insert(begin.call_id, idx);
            }
            EventMsg::McpToolCallEnd(end) => {
                let outcome = match &end.result {
                    Ok(_) => Ok(format!("ok in {:?}", end.duration)),
                    Err(err) => Err(err.clone()),
                };
                if let Some(idx) = self.lifecycle.pending_mut().calls.remove(&end.call_id)
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
                self.lifecycle.finish();
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

    fn push_stream_entry(&mut self, kind: StreamKind, item_id: String, delta: &str) {
        if let Some(idx) = self.lifecycle.pending().streams.get(&item_id).copied() {
            match (kind, self.transcript.get_mut(idx)) {
                (StreamKind::Agent, Some(TranscriptEntry::Agent { content }))
                | (StreamKind::Reasoning, Some(TranscriptEntry::Reasoning { content })) => {
                    content.push_str(delta);
                    return;
                }
                _ => {
                    self.lifecycle.pending_mut().streams.remove(&item_id);
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
        self.lifecycle.pending_mut().streams.insert(item_id, idx);
    }

    fn finalize_stream_entry(&mut self, kind: StreamKind, full: &str) {
        for idx in (0..self.transcript.len()).rev() {
            match &self.transcript[idx] {
                TranscriptEntry::User { .. } => break,
                TranscriptEntry::Agent { .. } if kind == StreamKind::Agent => {
                    self.transcript[idx] = TranscriptEntry::Agent {
                        content: full.to_string(),
                    };
                    self.lifecycle
                        .pending_mut()
                        .streams
                        .retain(|_, value| *value != idx);
                    return;
                }
                TranscriptEntry::Reasoning { .. } if kind == StreamKind::Reasoning => {
                    self.transcript[idx] = TranscriptEntry::Reasoning {
                        content: full.to_string(),
                    };
                    self.lifecycle
                        .pending_mut()
                        .streams
                        .retain(|_, value| *value != idx);
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
mod tests;
