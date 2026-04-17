//! `chaos-xclient` — the iced-based X11/Wayland GUI userland program.
//!
//! Peer to `chaos-console` (the TUI): both are userland binaries that talk
//! to the kernel through the same SQ/EQ surface exposed by `chaos-session`.
//! Neither is a "frontend" of the other — they are two independent programs
//! running on top of the same ABI, the way `xterm` and `GNOME Terminal` are
//! peers on top of POSIX.
//!
//! # Architecture
//!
//! [`run`] drives the full userland boot sequence:
//!
//! 1. Create a dedicated tokio runtime. iced brings its own executor for its
//!    UI tasks, but [`chaos_session::ClientSession`] and everything it calls
//!    into (`chaos-kern`) spawns on ambient tokio — so we need a runtime
//!    entered on the main thread before we hand control to iced.
//! 2. Run [`chaos_init::ChaosInit::boot`] to get an `AuthManager` +
//!    `ProcessTable` pair. Snapshot a [`TurnTemplate`] from the config so
//!    the composer can build [`Op::UserTurn`] values without holding a live
//!    `Config` reference. Spawn [`ClientSession`] to cold-start the kernel
//!    process and capture its `op_tx` / `event_rx` pair.
//! 3. Build the iced application, moving the template + `op_tx` into state
//!    and `event_rx` into a boot-time [`iced::Task::stream`] so kernel
//!    events are delivered to [`ChaosWindow::update`] as
//!    [`Message::KernelEvent`] variants.
//!
//! The view is a composer + a scrollable transcript. Submitting the
//! composer builds an `Op::UserTurn` from the template, pushes it into the
//! submission queue, and enters an "in-flight" state until the kernel emits
//! [`EventMsg::TurnComplete`]. Tasks #15 and #16 layer on proper markdown
//! rendering and theming.

#![warn(clippy::all)]

mod app;
mod chat;
mod state;
mod theme;
mod turn;

use std::sync::Arc;
use std::sync::Mutex;

use chaos_init::ChaosInit;
use chaos_ipc::protocol::ChaosErrorInfo;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::SessionSource;
use chaos_kern::config::ConfigBuilder;
use chaos_kern::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use chaos_session::ClientSession;
use iced::Task;
use tokio::sync::mpsc::UnboundedReceiver;

pub use state::ChaosWindow;
pub use theme::ANTHROPIC;
pub use theme::ChaosPalette;
pub use theme::PHOSPHOR;
pub use turn::TurnTemplate;

/// Messages produced by the view or the kernel bridge, consumed by
/// [`ChaosWindow::update`].
#[derive(Debug, Clone)]
pub enum Message {
    /// The composer text changed.
    ComposerChanged(String),
    /// The user hit enter or clicked Send.
    ComposerSubmit,
    /// The user asked the kernel to interrupt its current turn.
    Interrupt,
    /// A new [`Event`] arrived from the kernel.
    ///
    /// Boxed so the `Message` enum stays small — events can carry large
    /// payloads (full conversation snapshots, tool call results).
    KernelEvent(Box<Event>),
    /// The kernel event stream terminated. Fired exactly once by
    /// [`kernel_event_stream`] after the underlying receiver closes, even
    /// if no [`EventMsg::ShutdownComplete`] was observed. This is the GUI's
    /// only out-of-band signal that a silent kernel death has happened.
    KernelDisconnected,
    /// No-op sink for widget events we don't yet act on (currently: link
    /// clicks inside rendered markdown). A later pass will route these into
    /// a real handler; for now the update loop drops them.
    Nop,
    /// Toggle the clamped-mode flag — flips the palette between [`PHOSPHOR`]
    /// and [`ANTHROPIC`]. Keyed to a visible "phosphor/clamped" button so
    /// users can preview the Anthropic branding without editing a config.
    ToggleClamped,
}

/// Run the chaos window app.
///
/// Builds a tokio runtime, cold-starts a `ClientSession`, then hands control
/// to iced and blocks until the window closes or the kernel session
/// terminates. Returns [`anyhow::Result`] so the caller can distinguish
/// "kernel failed to boot" from "iced crashed" in the binary's exit code.
pub fn run() -> anyhow::Result<()> {
    // iced drives UI work on its own executor, but `ClientSession::spawn`
    // and the kernel call into `tokio::spawn`, so we need a runtime entered
    // on the main thread before iced takes over.
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();

    let (template, op_tx, event_rx) = runtime.block_on(async {
        let config = ConfigBuilder::default().build().await?;

        let init = ChaosInit::boot(
            &config,
            SessionSource::Cli,
            CollaborationModesConfig {
                default_mode_request_user_input: true,
            },
        );

        let template = TurnTemplate::from_config(&config);

        let session = ClientSession::spawn(
            config,
            init.process_table,
            Some("chaos-xclient".to_string()),
        );

        anyhow::Ok((template, session.op_tx, session.event_rx))
    })?;

    // iced's `BootFn` is `Fn`, not `FnOnce`, so the closure is callable more
    // than once in principle. In practice iced only calls it once per run,
    // but we still need interior mutability to move the receiver out — wrap
    // it in a Mutex<Option<_>> and take() on the first call.
    let handles = Arc::new(Mutex::new(Some((template, op_tx, event_rx))));

    iced::application(
        move || {
            let taken = handles.lock().ok().and_then(|mut guard| guard.take());
            // A second call (or a poisoned lock) is a logic bug: iced calls
            // `boot` exactly once per `run()`, and nothing else touches the
            // mutex. Scream in dev so regressions can't hide; in release we
            // still fall back to an inert window rather than crash the GUI.
            debug_assert!(
                taken.is_some(),
                "iced called boot more than once, or handles mutex poisoned"
            );
            if let Some((template, op_tx, event_rx)) = taken {
                let state = ChaosWindow::new(template, op_tx);
                let task = Task::stream(kernel_event_stream(event_rx));
                (state, task)
            } else {
                #[allow(clippy::print_stderr)]
                {
                    eprintln!("chaos-xclient: iced boot closure re-entered; window will be inert");
                }
                let (dead_tx, _) = tokio::sync::mpsc::unbounded_channel::<Op>();
                (
                    ChaosWindow::new(TurnTemplate::fallback(), dead_tx),
                    Task::none(),
                )
            }
        },
        ChaosWindow::update,
        ChaosWindow::view,
    )
    .title("chaos-xclient")
    .theme(ChaosWindow::theme)
    .run()?;

    // `runtime` lives until end of scope so the kernel-backed tasks keep a
    // tokio context for the lifetime of the GUI. No explicit drop needed.
    Ok(())
}

/// Bridge the kernel's `UnboundedReceiver<Event>` into a `Stream<Item = Message>`
/// that `iced::Task::stream` can drive.
///
/// Emits one [`Message::KernelEvent`] per kernel event, then a single
/// terminal [`Message::KernelDisconnected`] when the receiver closes
/// (kernel shutdown, `ClientSession` drop, kernel crash). Without that
/// sentinel a silent kernel death would leave the GUI wedged in
/// `TurnState::InFlight` with no signal to unlock the composer.
fn kernel_event_stream(
    event_rx: UnboundedReceiver<Event>,
) -> impl futures::Stream<Item = Message> + Send + 'static {
    use futures::StreamExt;
    let events = futures::stream::unfold(event_rx, |mut rx| async move {
        let event = rx.recv().await?;
        Some((Message::KernelEvent(Box::new(event)), rx))
    });
    events.chain(futures::stream::once(async { Message::KernelDisconnected }))
}

/// Render an `ErrorEvent`-style message + optional structured tag into a
/// human-readable single line. Structured tags become a prefix so the user
/// can see the category without digging through Debug.
fn format_error(message: &str, info: Option<&ChaosErrorInfo>) -> String {
    let tag: &str = match info {
        None => return message.to_string(),
        Some(ChaosErrorInfo::ContextWindowExceeded) => "context window exceeded",
        Some(ChaosErrorInfo::UsageLimitExceeded) => "usage limit reached",
        Some(ChaosErrorInfo::ServerOverloaded) => "server overloaded",
        Some(ChaosErrorInfo::Unauthorized) => "unauthorized",
        Some(ChaosErrorInfo::BadRequest) => "bad request",
        Some(ChaosErrorInfo::SandboxError) => "sandbox error",
        Some(ChaosErrorInfo::InternalServerError) => "internal server error",
        Some(ChaosErrorInfo::ProcessRollbackFailed) => "process rollback failed",
        Some(ChaosErrorInfo::Other) => "error",
        // HTTP-ish structured variants: include the status code when we
        // have one so the user can see "http 503" instead of "Debug".
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
    use std::path::PathBuf;
    use std::time::Duration;

    use chaos_ipc::ProcessId;
    use chaos_ipc::config_types::ApprovalsReviewer;
    use chaos_ipc::protocol::AgentMessageContentDeltaEvent;
    use chaos_ipc::protocol::AgentMessageEvent;
    use chaos_ipc::protocol::AgentReasoningEvent;
    use chaos_ipc::protocol::ApprovalPolicy;
    use chaos_ipc::protocol::BackgroundEventEvent;
    use chaos_ipc::protocol::ChaosErrorInfo;
    use chaos_ipc::protocol::ErrorEvent;
    use chaos_ipc::protocol::EventMsg;
    use chaos_ipc::protocol::ExecCommandSource;
    use chaos_ipc::protocol::ExecCommandStatus;
    use chaos_ipc::protocol::McpInvocation;
    use chaos_ipc::protocol::ReasoningContentDeltaEvent;
    use chaos_ipc::protocol::SandboxPolicy;
    use chaos_ipc::protocol::SessionConfiguredEvent;
    use chaos_ipc::protocol::StreamErrorEvent;
    use chaos_ipc::protocol::TokenCountEvent;
    use chaos_ipc::protocol::TokenUsage;
    use chaos_ipc::protocol::TokenUsageInfo;
    use chaos_ipc::protocol::TurnAbortReason;
    use chaos_ipc::protocol::TurnAbortedEvent;
    use chaos_ipc::protocol::TurnCompleteEvent;
    use chaos_ipc::protocol::WarningEvent;
    use chaos_ipc::user_input::UserInput;
    use futures::StreamExt;
    use tokio::sync::mpsc::unbounded_channel;

    use iced::Theme;

    use super::*;
    use crate::chat::ChatEntry;
    use crate::chat::NoticeLevel;
    use crate::state::Status;
    use crate::state::TurnState;

    fn mk_window() -> (ChaosWindow, tokio::sync::mpsc::UnboundedReceiver<Op>) {
        let (op_tx, op_rx) = unbounded_channel::<Op>();
        (ChaosWindow::new(TurnTemplate::fallback(), op_tx), op_rx)
    }

    fn kernel_event(msg: EventMsg) -> Message {
        Message::KernelEvent(Box::new(Event {
            id: "t".to_string(),
            msg,
        }))
    }

    fn count_notices(app: &ChaosWindow, want: NoticeLevel) -> usize {
        app.transcript
            .iter()
            .filter(|e| matches!(e, ChatEntry::Notice { level, .. } if *level == want))
            .count()
    }

    /// Minimal `SessionConfiguredEvent` stub for tests — only the fields the
    /// GUI actually reads need to be sensible; everything else is filled
    /// with defaults that pass deserialization round-trips.
    fn session_configured() -> SessionConfiguredEvent {
        SessionConfiguredEvent {
            session_id: ProcessId::default(),
            forked_from_id: None,
            process_name: None,
            model: String::new(),
            model_provider_id: "chaos-xclient-test".to_string(),
            service_tier: None,
            approval_policy: ApprovalPolicy::default(),
            approvals_reviewer: ApprovalsReviewer::default(),
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            cwd: PathBuf::from("/"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
        }
    }

    /// Full happy-path turn: SessionConfigured flips Ready, composer gathers
    /// text, submit pushes a well-formed `Op::UserTurn` onto `op_rx`, an
    /// AgentMessage lands in the transcript, TurnComplete releases the
    /// composer, and Interrupt is gated off (Idle state). One dense test
    /// per the "fewer dense tests" guidance.
    #[test]
    fn full_turn_roundtrip() {
        let (mut app, mut op_rx) = mk_window();

        // Composer disabled until Ready.
        app.update(kernel_event(EventMsg::SessionConfigured(
            session_configured(),
        )));
        assert_eq!(app.status, Status::Ready);
        assert!(app.can_submit());

        // Type + submit.
        app.update(Message::ComposerChanged("hello".to_string()));
        app.update(Message::ComposerSubmit);
        assert_eq!(app.turn, TurnState::InFlight);
        assert_eq!(app.transcript.len(), 1);
        match &app.transcript[0] {
            ChatEntry::User { text } => assert_eq!(text, "hello"),
            other => panic!("expected User entry, got {other:?}"),
        }
        assert_eq!(app.composer, "");

        // The submission queue saw a real UserTurn.
        match op_rx.try_recv() {
            Ok(Op::UserTurn { items, model, .. }) => {
                assert!(model.is_empty());
                assert_eq!(items.len(), 1);
                assert!(matches!(&items[0], UserInput::Text { text, .. } if text == "hello"));
            }
            other => panic!("expected UserTurn, got {other:?}"),
        }

        // Can't submit while in flight.
        app.update(Message::ComposerChanged("second".to_string()));
        app.update(Message::ComposerSubmit);
        assert_eq!(app.transcript.len(), 1, "second submit should be gated");
        assert_eq!(app.composer, "second", "composer text preserved");

        // Agent reply lands in the transcript.
        app.update(kernel_event(EventMsg::AgentMessage(AgentMessageEvent {
            message: "hi there".to_string(),
            phase: None,
        })));
        assert_eq!(app.transcript.len(), 2);
        assert!(matches!(&app.transcript[1], ChatEntry::Agent { .. }));

        // TurnComplete flips back to Idle and unblocks the composer.
        app.update(kernel_event(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "t1".to_string(),
            last_agent_message: Some("hi there".to_string()),
        })));
        assert_eq!(app.turn, TurnState::Idle);
        assert!(app.can_submit());
    }

    /// Every way a turn can end besides `TurnComplete`: Interrupt waits for
    /// `TurnAborted` before releasing InFlight, `Error` releases InFlight on
    /// its own, `KernelDisconnected` marks the session dead even without a
    /// `ShutdownComplete`, and submission after death is a no-op.
    #[test]
    fn interrupt_error_and_disconnect_paths() {
        let (mut app, mut op_rx) = mk_window();
        app.update(kernel_event(EventMsg::SessionConfigured(
            session_configured(),
        )));

        // --- Interrupt path: send, stay InFlight, only release on TurnAborted.
        app.update(Message::ComposerChanged("go".to_string()));
        app.update(Message::ComposerSubmit);
        assert_eq!(app.turn, TurnState::InFlight);
        assert!(matches!(op_rx.try_recv(), Ok(Op::UserTurn { .. })));

        app.update(Message::Interrupt);
        assert_eq!(
            app.turn,
            TurnState::InFlight,
            "Interrupt must not release InFlight — wait for kernel terminal event"
        );
        assert!(matches!(op_rx.try_recv(), Ok(Op::Interrupt)));
        // Composer still gated until the kernel confirms the abort.
        app.update(Message::ComposerChanged("early".to_string()));
        app.update(Message::ComposerSubmit);
        assert!(op_rx.try_recv().is_err());

        app.update(kernel_event(EventMsg::TurnAborted(TurnAbortedEvent {
            turn_id: Some("t1".to_string()),
            reason: TurnAbortReason::Interrupted,
        })));
        assert_eq!(app.turn, TurnState::Idle);
        assert!(app.can_submit());

        // --- Error path: submit again, then land an Error — InFlight released.
        app.update(Message::ComposerChanged("again".to_string()));
        app.update(Message::ComposerSubmit);
        assert_eq!(app.turn, TurnState::InFlight);
        assert!(matches!(op_rx.try_recv(), Ok(Op::UserTurn { .. })));

        app.update(kernel_event(EventMsg::Error(ErrorEvent {
            message: "boom".to_string(),
            chaos_error_info: Some(ChaosErrorInfo::ContextWindowExceeded),
        })));
        assert_eq!(app.turn, TurnState::Idle);
        assert!(
            app.pending_streams.is_empty(),
            "terminal Error must clear orphaned streaming state"
        );
        assert!(
            app.pending_calls.is_empty(),
            "terminal Error must clear orphaned exec/tool bookkeeping"
        );
        match app.transcript.last() {
            Some(ChatEntry::Notice {
                level: NoticeLevel::Error,
                text,
            }) => {
                assert!(
                    text.contains("context window exceeded"),
                    "structured error info should prefix the message: {text}"
                );
                assert!(text.contains("boom"));
            }
            other => panic!("expected error notice, got {other:?}"),
        }

        // --- KernelDisconnected path: no ShutdownComplete, but the stream
        // sentinel still flips Status to Shutdown and logs one error notice.
        let before = count_notices(&app, NoticeLevel::Error);
        app.update(Message::KernelDisconnected);
        assert_eq!(app.status, Status::Shutdown);
        assert_eq!(app.turn, TurnState::Idle);
        let after = count_notices(&app, NoticeLevel::Error);
        assert_eq!(after, before + 1);

        // A second disconnect after we already know the kernel is gone must
        // not re-log — `ShutdownComplete`-or-equivalent is idempotent.
        app.update(Message::KernelDisconnected);
        assert_eq!(count_notices(&app, NoticeLevel::Error), after);

        // Submissions after shutdown are dropped on the floor.
        let len_before = app.transcript.len();
        app.update(Message::ComposerChanged("late".to_string()));
        app.update(Message::ComposerSubmit);
        assert_eq!(app.transcript.len(), len_before);
    }

    /// Every rich-rendering path #15 introduces, exercised in one pass:
    /// streaming agent deltas coalesce then finalize, reasoning deltas
    /// render as a separate entry kind, exec begin/end pair up, MCP tool
    /// calls pair up, warnings/background events render with their proper
    /// level, and `TokenCount` feeds the header without cluttering the
    /// transcript. Terminal events still drain pending-stream state so the
    /// next turn starts clean.
    #[test]
    fn rich_event_rendering() {
        let (mut app, _op_rx) = mk_window();
        app.update(kernel_event(EventMsg::SessionConfigured(
            session_configured(),
        )));

        // --- Streaming agent deltas keyed by item_id coalesce in place.
        let item_id = "item-1".to_string();
        app.update(kernel_event(EventMsg::AgentMessageContentDelta(
            AgentMessageContentDeltaEvent {
                process_id: "p".to_string(),
                turn_id: "t".to_string(),
                item_id: item_id.clone(),
                delta: "hello ".to_string(),
            },
        )));
        app.update(kernel_event(EventMsg::AgentMessageContentDelta(
            AgentMessageContentDeltaEvent {
                process_id: "p".to_string(),
                turn_id: "t".to_string(),
                item_id,
                delta: "world".to_string(),
            },
        )));
        // One in-progress Agent entry, tracked by item_id.
        assert_eq!(app.transcript.len(), 1);
        assert!(matches!(&app.transcript[0], ChatEntry::Agent { .. }));
        assert_eq!(app.pending_streams.len(), 1);

        // Finalize: overwrites the streamed entry in place, no duplicate.
        app.update(kernel_event(EventMsg::AgentMessage(AgentMessageEvent {
            message: "hello **world**".to_string(),
            phase: None,
        })));
        assert_eq!(
            app.transcript.len(),
            1,
            "AgentMessage after deltas must not create a second entry"
        );
        assert!(
            app.pending_streams.is_empty(),
            "finalize clears pending key"
        );

        // --- Reasoning deltas render as a separate entry kind.
        app.update(kernel_event(EventMsg::ReasoningContentDelta(
            ReasoningContentDeltaEvent {
                process_id: "p".to_string(),
                turn_id: "t".to_string(),
                item_id: "r-1".to_string(),
                delta: "thinking…".to_string(),
                summary_index: 0,
            },
        )));
        app.update(kernel_event(EventMsg::AgentReasoning(
            AgentReasoningEvent {
                text: "thought".to_string(),
            },
        )));
        assert!(matches!(&app.transcript[1], ChatEntry::Reasoning { .. }));

        // --- Exec command begin/end pair up on call_id.
        use chaos_ipc::protocol::ExecCommandBeginEvent;
        use chaos_ipc::protocol::ExecCommandEndEvent;
        app.update(kernel_event(EventMsg::ExecCommandBegin(
            ExecCommandBeginEvent {
                call_id: "exec-1".to_string(),
                process_id: None,
                turn_id: "t".to_string(),
                command: vec!["ls".to_string(), "-la".to_string()],
                cwd: PathBuf::from("/tmp"),
                parsed_cmd: Vec::new(),
                source: ExecCommandSource::default(),
                interaction_input: None,
            },
        )));
        // Exec entry starts as "running".
        match app.transcript.last() {
            Some(ChatEntry::Exec {
                exit_code: None,
                command,
                ..
            }) => {
                assert_eq!(command, &vec!["ls".to_string(), "-la".to_string()]);
            }
            other => panic!("expected running Exec entry, got {other:?}"),
        }
        app.update(kernel_event(EventMsg::ExecCommandEnd(
            ExecCommandEndEvent {
                call_id: "exec-1".to_string(),
                process_id: None,
                turn_id: "t".to_string(),
                command: vec!["ls".to_string(), "-la".to_string()],
                cwd: PathBuf::from("/tmp"),
                parsed_cmd: Vec::new(),
                source: ExecCommandSource::default(),
                interaction_input: None,
                stdout: String::new(),
                stderr: String::new(),
                aggregated_output: "total 0\n".to_string(),
                exit_code: 0,
                duration: Duration::from_millis(5),
                formatted_output: "total 0\n".to_string(),
                status: ExecCommandStatus::Completed,
            },
        )));
        // Same entry is flipped in place — not appended.
        match app.transcript.last() {
            Some(ChatEntry::Exec {
                exit_code: Some(0),
                output,
                ..
            }) => assert_eq!(output, "total 0\n"),
            other => panic!("expected completed Exec entry, got {other:?}"),
        }
        assert!(
            app.pending_calls.is_empty(),
            "exec end clears pending call_id"
        );

        // --- MCP tool call begin/end pair up on call_id.
        use chaos_ipc::protocol::McpToolCallBeginEvent;
        use chaos_ipc::protocol::McpToolCallEndEvent;
        app.update(kernel_event(EventMsg::McpToolCallBegin(
            McpToolCallBeginEvent {
                call_id: "tool-1".to_string(),
                invocation: McpInvocation {
                    server: "chaos".to_string(),
                    tool: "ping".to_string(),
                    arguments: None,
                },
            },
        )));
        app.update(kernel_event(EventMsg::McpToolCallEnd(
            McpToolCallEndEvent {
                call_id: "tool-1".to_string(),
                invocation: McpInvocation {
                    server: "chaos".to_string(),
                    tool: "ping".to_string(),
                    arguments: None,
                },
                duration: Duration::from_millis(12),
                result: Err("timeout".to_string()),
            },
        )));
        match app.transcript.last() {
            Some(ChatEntry::Tool {
                result: Some(Err(r)),
                ..
            }) => {
                assert!(r.contains("timeout"), "tool error text should propagate");
            }
            other => panic!("expected completed Tool entry with Err, got {other:?}"),
        }

        // --- Warning + Background render as distinct notice levels.
        app.update(kernel_event(EventMsg::Warning(WarningEvent {
            message: "watch out".to_string(),
        })));
        app.update(kernel_event(EventMsg::BackgroundEvent(
            BackgroundEventEvent {
                message: "indexing".to_string(),
            },
        )));
        assert!(count_notices(&app, NoticeLevel::Warn) >= 1);
        assert!(count_notices(&app, NoticeLevel::Info) >= 1);

        // --- TokenCount feeds the header, does NOT push a transcript entry.
        let len_before = app.transcript.len();
        app.update(kernel_event(EventMsg::TokenCount(TokenCountEvent {
            info: Some(TokenUsageInfo {
                total_token_usage: TokenUsage {
                    input_tokens: 10,
                    cached_input_tokens: 0,
                    output_tokens: 20,
                    reasoning_output_tokens: 0,
                    total_tokens: 30,
                },
                last_token_usage: TokenUsage {
                    input_tokens: 0,
                    cached_input_tokens: 0,
                    output_tokens: 0,
                    reasoning_output_tokens: 0,
                    total_tokens: 0,
                },
                model_context_window: None,
            }),
            rate_limits: None,
        })));
        assert_eq!(app.transcript.len(), len_before, "TokenCount must not log");
        assert!(app.token_usage.is_some());
        assert!(app.header_text().contains("total:30"));
    }

    /// Regression coverage for every scenario both reviewers flagged as a
    /// blocker or test gap for #15. Kept as one dense test per the
    /// "fewer dense tests" preference — each stanza is a different bug
    /// class:
    ///
    /// 1. Late `AgentMessage` arriving *after* `TurnComplete` must finalize
    ///    the streamed entry in place, not append a duplicate.
    /// 2. `pending_calls` must not leak across turns — an orphaned
    ///    `ExecCommandBegin` whose end never arrives gets dropped when
    ///    the turn ends, and a recycled `call_id` on the next turn must
    ///    land as a fresh entry.
    /// 3. Orphan `ExecCommandEnd` with stderr-only output must preserve
    ///    the stderr, not drop it.
    /// 4. `format_error`'s HTTP variants surface `http {code}` and the
    ///    structured tag, never the Debug fallback.
    /// 5. `StreamError` stays `InFlight` (kernel is retrying) and renders
    ///    as a `Warn` notice, distinct from a terminal `Error`.
    #[test]
    fn finalize_reconciliation_and_call_leaks() {
        use chaos_ipc::protocol::ExecCommandBeginEvent;
        use chaos_ipc::protocol::ExecCommandEndEvent;

        // --- (1) Late finalize after TurnComplete --------------------
        let (mut app, _rx) = mk_window();
        app.update(kernel_event(EventMsg::SessionConfigured(
            session_configured(),
        )));
        app.update(Message::ComposerChanged("hi".to_string()));
        app.update(Message::ComposerSubmit);
        app.update(kernel_event(EventMsg::AgentMessageContentDelta(
            AgentMessageContentDeltaEvent {
                process_id: "p".to_string(),
                turn_id: "t".to_string(),
                item_id: "agent-1".to_string(),
                delta: "partial".to_string(),
            },
        )));
        // Transcript: [User, Agent(partial)]
        assert_eq!(app.transcript.len(), 2);

        // TurnComplete clears pending_streams *before* the finalize arrives.
        app.update(kernel_event(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "t1".to_string(),
            last_agent_message: Some("finalized".to_string()),
        })));
        assert!(app.pending_streams.is_empty());
        assert!(app.pending_calls.is_empty());

        // Late AgentMessage must finalize in place, not duplicate.
        app.update(kernel_event(EventMsg::AgentMessage(AgentMessageEvent {
            message: "finalized".to_string(),
            phase: None,
        })));
        assert_eq!(
            app.transcript.len(),
            2,
            "late AgentMessage must finalize in place, not duplicate"
        );
        assert!(matches!(&app.transcript[1], ChatEntry::Agent { .. }));

        // --- (2) pending_calls leak across turns ---------------------
        // Exec that never gets an end event before the turn ends.
        app.update(Message::ComposerChanged("next".to_string()));
        app.update(Message::ComposerSubmit);
        app.update(kernel_event(EventMsg::ExecCommandBegin(
            ExecCommandBeginEvent {
                call_id: "call-X".to_string(),
                process_id: None,
                turn_id: "t2".to_string(),
                command: vec!["sleep".to_string(), "9".to_string()],
                cwd: PathBuf::from("/tmp"),
                parsed_cmd: Vec::new(),
                source: ExecCommandSource::default(),
                interaction_input: None,
            },
        )));
        assert_eq!(
            app.pending_calls.len(),
            1,
            "begin registered in pending_calls"
        );

        app.update(kernel_event(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "t2".to_string(),
            last_agent_message: None,
        })));
        assert!(
            app.pending_calls.is_empty(),
            "TurnComplete must drop orphaned pending_calls to prevent call_id leaks"
        );

        // Next turn: a recycled call_id lands as a *new* exec entry
        // instead of mutating the orphaned one from the previous turn.
        let idx_before = app.transcript.len();
        app.update(Message::ComposerChanged("third".to_string()));
        app.update(Message::ComposerSubmit);
        app.update(kernel_event(EventMsg::ExecCommandBegin(
            ExecCommandBeginEvent {
                call_id: "call-X".to_string(),
                process_id: None,
                turn_id: "t3".to_string(),
                command: vec!["echo".to_string(), "hi".to_string()],
                cwd: PathBuf::from("/tmp"),
                parsed_cmd: Vec::new(),
                source: ExecCommandSource::default(),
                interaction_input: None,
            },
        )));
        // Fresh entry appended (idx_before pointed at the user submit).
        assert!(app.transcript.len() > idx_before + 1);
        match app.transcript.last() {
            Some(ChatEntry::Exec {
                exit_code: None,
                command,
                ..
            }) => assert_eq!(command[0], "echo"),
            other => panic!("expected running Exec entry, got {other:?}"),
        }

        // --- (3) Orphan ExecCommandEnd preserves stderr ---------------
        app.update(kernel_event(EventMsg::ExecCommandEnd(
            ExecCommandEndEvent {
                call_id: "unknown-call".to_string(),
                process_id: None,
                turn_id: "t3".to_string(),
                command: vec!["cargo".to_string(), "build".to_string()],
                cwd: PathBuf::from("/tmp"),
                parsed_cmd: Vec::new(),
                source: ExecCommandSource::default(),
                interaction_input: None,
                stdout: String::new(),
                stderr: "linker error".to_string(),
                aggregated_output: String::new(),
                exit_code: 1,
                duration: Duration::from_millis(1),
                formatted_output: String::new(),
                status: ExecCommandStatus::Failed,
            },
        )));
        // Orphan fallback: appended as a standalone entry with stderr
        // rendered into `output` via the aggregated→stdout→stderr chain.
        match app.transcript.last() {
            Some(ChatEntry::Exec {
                exit_code: Some(1),
                output,
                ..
            }) => assert_eq!(output, "linker error"),
            other => panic!("expected orphan Exec entry with stderr, got {other:?}"),
        }

        // --- (4) format_error HTTP variants ---------------------------
        assert_eq!(
            format_error("nope", None),
            "nope",
            "None info must pass message through unchanged"
        );
        let with_status = format_error(
            "boom",
            Some(&ChaosErrorInfo::HttpConnectionFailed {
                http_status_code: Some(503),
            }),
        );
        assert!(with_status.contains("http 503"));
        assert!(with_status.contains("boom"));
        let without_status = format_error(
            "boom",
            Some(&ChaosErrorInfo::ResponseStreamDisconnected {
                http_status_code: None,
            }),
        );
        assert!(without_status.contains("response stream disconnected"));
        assert!(
            !without_status.contains("http "),
            "missing status code must not render 'http None'"
        );

        // --- (5) StreamError stays InFlight, renders as Warn ---------
        let (mut app2, _rx2) = mk_window();
        app2.update(kernel_event(EventMsg::SessionConfigured(
            session_configured(),
        )));
        app2.update(Message::ComposerChanged("run".to_string()));
        app2.update(Message::ComposerSubmit);
        assert_eq!(app2.turn, TurnState::InFlight);
        app2.update(kernel_event(EventMsg::StreamError(StreamErrorEvent {
            message: "upstream reset".to_string(),
            chaos_error_info: None,
            additional_details: None,
        })));
        assert_eq!(
            app2.turn,
            TurnState::InFlight,
            "StreamError must not release InFlight — the kernel is still retrying"
        );
        match app2.transcript.last() {
            Some(ChatEntry::Notice {
                level: NoticeLevel::Warn,
                text,
            }) => assert!(text.contains("upstream reset")),
            other => panic!("expected Warn notice, got {other:?}"),
        }
    }

    /// Regressions for the remaining xclient lifecycle edges:
    ///
    /// 1. `EventMsg::Error` is terminal for the turn, so any in-flight
    ///    streaming/item and exec/tool bookkeeping must be dropped just like
    ///    `TurnComplete` / `TurnAborted`.
    /// 2. If the kernel disappears before `op_tx.send(op)` succeeds, the
    ///    draft was never submitted and must stay in the composer.
    #[test]
    fn terminal_error_cleans_bookkeeping_and_failed_submit_keeps_draft() {
        use chaos_ipc::protocol::ExecCommandBeginEvent;
        use chaos_ipc::protocol::ExecCommandEndEvent;
        use chaos_ipc::protocol::McpToolCallBeginEvent;

        // --- (1) Error clears pending stream/call state -----------------
        let (mut app, _rx) = mk_window();
        app.update(kernel_event(EventMsg::SessionConfigured(
            session_configured(),
        )));
        app.update(Message::ComposerChanged("go".to_string()));
        app.update(Message::ComposerSubmit);

        app.update(kernel_event(EventMsg::AgentMessageContentDelta(
            AgentMessageContentDeltaEvent {
                process_id: "p".to_string(),
                turn_id: "t".to_string(),
                item_id: "agent-err".to_string(),
                delta: "partial".to_string(),
            },
        )));
        app.update(kernel_event(EventMsg::ExecCommandBegin(
            ExecCommandBeginEvent {
                call_id: "exec-err".to_string(),
                process_id: None,
                turn_id: "t".to_string(),
                command: vec!["false".to_string()],
                cwd: PathBuf::from("/tmp"),
                parsed_cmd: Vec::new(),
                source: ExecCommandSource::default(),
                interaction_input: None,
            },
        )));
        app.update(kernel_event(EventMsg::McpToolCallBegin(
            McpToolCallBeginEvent {
                call_id: "tool-err".to_string(),
                invocation: McpInvocation {
                    server: "chaos".to_string(),
                    tool: "ping".to_string(),
                    arguments: None,
                },
            },
        )));
        assert!(
            !app.pending_streams.is_empty(),
            "delta should register pending stream state"
        );
        assert_eq!(
            app.pending_calls.len(),
            2,
            "exec + tool begin should register both pending call ids"
        );

        app.update(kernel_event(EventMsg::Error(ErrorEvent {
            message: "boom".to_string(),
            chaos_error_info: None,
        })));
        assert_eq!(app.turn, TurnState::Idle);
        assert!(
            app.pending_streams.is_empty(),
            "terminal Error must clear pending stream state"
        );
        assert!(
            app.pending_calls.is_empty(),
            "terminal Error must clear pending exec/tool state"
        );

        // Recycled ids after the error should create fresh entries instead of
        // mutating the stale ones from the aborted turn.
        let len_before = app.transcript.len();
        app.update(kernel_event(EventMsg::ExecCommandEnd(
            ExecCommandEndEvent {
                call_id: "exec-err".to_string(),
                process_id: None,
                turn_id: "t".to_string(),
                command: vec!["echo".to_string(), "late".to_string()],
                cwd: PathBuf::from("/tmp"),
                parsed_cmd: Vec::new(),
                source: ExecCommandSource::default(),
                interaction_input: None,
                stdout: "late\n".to_string(),
                stderr: String::new(),
                aggregated_output: String::new(),
                exit_code: 0,
                duration: Duration::from_millis(1),
                formatted_output: "late\n".to_string(),
                status: ExecCommandStatus::Completed,
            },
        )));
        assert_eq!(
            app.transcript.len(),
            len_before + 1,
            "late end after terminal Error should append an orphan entry, not mutate stale state"
        );

        // --- (2) failed submit keeps the draft --------------------------
        let (mut app2, op_rx2) = mk_window();
        app2.update(kernel_event(EventMsg::SessionConfigured(
            session_configured(),
        )));
        app2.update(Message::ComposerChanged("keep me".to_string()));
        drop(op_rx2);

        let transcript_len = app2.transcript.len();
        app2.update(Message::ComposerSubmit);

        assert_eq!(
            app2.composer, "keep me",
            "failed send must not discard an unsent draft"
        );
        assert_eq!(
            app2.transcript.len(),
            transcript_len + 1,
            "kernel death should log one shutdown notice and no User entry"
        );
        assert!(matches!(
            app2.transcript.last(),
            Some(ChatEntry::Notice {
                level: NoticeLevel::Error,
                ..
            })
        ));
        assert_eq!(app2.status, Status::Shutdown);
        assert_eq!(app2.turn, TurnState::Idle);
    }

    /// Everything #16 brought in, exercised in one dense pass: palette
    /// slot identity, iced-palette projection, theme variant + stable name,
    /// `ToggleClamped` flipping PHOSPHOR ↔ ANTHROPIC, and `view()` not
    /// panicking in either mode with a transcript that covers every
    /// `ChatEntry` kind (the real regression class — the render pass
    /// reaches into every arm, and iced will assert on inconsistent Length
    /// / Style at build time).
    #[test]
    fn theme_palette_and_view_smoke() {
        use chaos_ipc::protocol::ExecCommandBeginEvent;
        use chaos_ipc::protocol::ExecCommandEndEvent;
        use chaos_ipc::protocol::McpToolCallBeginEvent;
        use chaos_ipc::protocol::McpToolCallEndEvent;

        // --- (1) Palette slot identity ------------------------------------
        // Phosphor keeps distinct roles for error/warning/accent — these
        // must not collapse onto each other, otherwise the view loses
        // semantic information.
        assert_ne!(PHOSPHOR.error, PHOSPHOR.warning);
        assert_ne!(PHOSPHOR.bg, PHOSPHOR.fg);
        assert_ne!(PHOSPHOR.accent, PHOSPHOR.fg);
        assert_ne!(PHOSPHOR.fg, PHOSPHOR.dim);
        // Fidelity to `bin/console/src/theme.rs`: the PHOSPHOR palette
        // there uses a single ratatui `Color::Green` for dim, border, and
        // success. Mirror that — three slots, one shade.
        assert_eq!(PHOSPHOR.dim, PHOSPHOR.border);
        assert_eq!(PHOSPHOR.dim, PHOSPHOR.success);
        // `fg == highlight` is the other console equality (both `LightGreen`).
        assert_eq!(PHOSPHOR.fg, PHOSPHOR.highlight);
        // ANTHROPIC mirrors console's collapsed role set: dim, highlight,
        // border, warning, and accent are all a single `Color::Yellow`.
        assert_eq!(ANTHROPIC.dim, ANTHROPIC.highlight);
        assert_eq!(ANTHROPIC.dim, ANTHROPIC.border);
        assert_eq!(ANTHROPIC.dim, ANTHROPIC.warning);
        assert_eq!(ANTHROPIC.dim, ANTHROPIC.accent);
        // `fg == success` is console's other ANTHROPIC equality (both `LightYellow`).
        assert_eq!(ANTHROPIC.fg, ANTHROPIC.success);
        // Anthropic is warm: fg must not be green (red > blue).
        const { assert!(ANTHROPIC.fg.r > ANTHROPIC.fg.b) };

        // --- (2) iced-palette projection keeps the right slots -----------
        let ip = PHOSPHOR.to_iced_palette();
        assert_eq!(ip.background, PHOSPHOR.bg);
        assert_eq!(ip.text, PHOSPHOR.fg);
        assert_eq!(ip.primary, PHOSPHOR.highlight);
        assert_eq!(ip.success, PHOSPHOR.success);
        assert_eq!(ip.warning, PHOSPHOR.warning);
        assert_eq!(ip.danger, PHOSPHOR.error);

        // --- (3) Theme is Custom + stable name per palette ---------------
        let theme = PHOSPHOR.to_theme("chaos-phosphor");
        match &theme {
            Theme::Custom(_) => {}
            other => panic!("expected Theme::Custom, got {other:?}"),
        }
        // Palette round-trips through the built Theme.
        assert_eq!(theme.palette(), PHOSPHOR.to_iced_palette());
        let clamped = ANTHROPIC.to_theme("chaos-anthropic");
        assert_eq!(clamped.palette(), ANTHROPIC.to_iced_palette());
        assert_ne!(theme.palette(), clamped.palette());

        // --- (4) ToggleClamped flips palette + theme selection -----------
        let (mut app, _rx) = mk_window();
        assert!(!app.clamped);
        assert_eq!(app.palette(), PHOSPHOR);
        assert_eq!(app.theme().palette(), PHOSPHOR.to_iced_palette());

        app.update(Message::ToggleClamped);
        assert!(app.clamped);
        assert_eq!(app.palette(), ANTHROPIC);
        assert_eq!(app.theme().palette(), ANTHROPIC.to_iced_palette());

        app.update(Message::ToggleClamped);
        assert!(!app.clamped);
        assert_eq!(app.palette(), PHOSPHOR);

        // --- (5) view() smoke: build a transcript covering every entry
        // kind, then render in both palette modes. Dropping the Element
        // is the real assertion — `Element<'_, Message>` is opaque, but
        // building it runs through every styled arm of `render_entry`
        // plus the root/transcript container styles. Any panic, lifetime
        // flub, or `Length` mismatch would surface here.
        app.update(kernel_event(EventMsg::SessionConfigured(
            session_configured(),
        )));
        app.update(Message::ComposerChanged("hello".to_string()));
        app.update(Message::ComposerSubmit);
        // AgentMessage finalizes in place — covers the Agent + markdown arm.
        app.update(kernel_event(EventMsg::AgentMessage(AgentMessageEvent {
            message: "ok **bold** [link](https://x)".to_string(),
            phase: None,
        })));
        // AgentReasoning exercises the Reasoning arm.
        app.update(kernel_event(EventMsg::AgentReasoning(
            AgentReasoningEvent {
                text: "thought".to_string(),
            },
        )));
        // Three notice levels + a warning + a background event.
        app.update(kernel_event(EventMsg::Warning(WarningEvent {
            message: "caution".to_string(),
        })));
        app.update(kernel_event(EventMsg::BackgroundEvent(
            BackgroundEventEvent {
                message: "indexing".to_string(),
            },
        )));
        app.update(kernel_event(EventMsg::Error(ErrorEvent {
            message: "boom".to_string(),
            chaos_error_info: Some(ChaosErrorInfo::SandboxError),
        })));
        // Exec (completed + failed) + Tool (ok + error) cover the code arms.
        app.update(kernel_event(EventMsg::ExecCommandBegin(
            ExecCommandBeginEvent {
                call_id: "e1".to_string(),
                process_id: None,
                turn_id: "t".to_string(),
                command: vec!["ls".to_string(), "-la".to_string()],
                cwd: PathBuf::from("/tmp"),
                parsed_cmd: Vec::new(),
                source: ExecCommandSource::default(),
                interaction_input: None,
            },
        )));
        app.update(kernel_event(EventMsg::ExecCommandEnd(
            ExecCommandEndEvent {
                call_id: "e1".to_string(),
                process_id: None,
                turn_id: "t".to_string(),
                command: vec!["ls".to_string()],
                cwd: PathBuf::from("/tmp"),
                parsed_cmd: Vec::new(),
                source: ExecCommandSource::default(),
                interaction_input: None,
                stdout: "file\n".to_string(),
                stderr: String::new(),
                aggregated_output: "file\n".to_string(),
                exit_code: 0,
                duration: Duration::from_millis(1),
                formatted_output: "file\n".to_string(),
                status: ExecCommandStatus::Completed,
            },
        )));
        app.update(kernel_event(EventMsg::McpToolCallBegin(
            McpToolCallBeginEvent {
                call_id: "t1".to_string(),
                invocation: McpInvocation {
                    server: "chaos".to_string(),
                    tool: "ping".to_string(),
                    arguments: None,
                },
            },
        )));
        app.update(kernel_event(EventMsg::McpToolCallEnd(
            McpToolCallEndEvent {
                call_id: "t1".to_string(),
                invocation: McpInvocation {
                    server: "chaos".to_string(),
                    tool: "ping".to_string(),
                    arguments: None,
                },
                duration: Duration::from_millis(3),
                result: Ok(chaos_ipc::mcp::CallToolResult {
                    content: Vec::new(),
                    structured_content: None,
                    is_error: None,
                    meta: None,
                }),
            },
        )));
        // Confirm the tool landed as structural `Ok(..)` — the view arm
        // discriminates on the Result, not on stringified text.
        assert!(
            app.transcript.iter().any(|e| matches!(
                e,
                ChatEntry::Tool {
                    result: Some(Ok(_)),
                    ..
                }
            )),
            "tool success branch must land as Ok for view to color it green"
        );
        // Still-running exec, covering the `None` exit_code arm.
        app.update(kernel_event(EventMsg::ExecCommandBegin(
            ExecCommandBeginEvent {
                call_id: "e2".to_string(),
                process_id: None,
                turn_id: "t".to_string(),
                command: vec!["sleep".to_string(), "1".to_string()],
                cwd: PathBuf::from("/tmp"),
                parsed_cmd: Vec::new(),
                source: ExecCommandSource::default(),
                interaction_input: None,
            },
        )));

        // Render in phosphor mode, then clamp and render again. The assert
        // is that neither call panics and both produce a live Element.
        // Drop each Element before the next mutable op so the app borrow
        // lifetime from `view()` doesn't overlap the update.
        drop(app.view());
        app.update(Message::ToggleClamped);
        assert!(app.clamped);
        drop(app.view());
    }

    /// `kernel_event_stream` bridges a tokio mpsc receiver into iced's
    /// `Task::stream`. Verify each event is pumped through, and — the whole
    /// point of the sentinel — when the senders drop, the stream emits one
    /// final `KernelDisconnected` before terminating. Without that terminal
    /// message a silent kernel death would leave the GUI wedged.
    #[tokio::test]
    async fn kernel_event_stream_pumps_then_emits_sentinel() {
        use chaos_ipc::protocol::EventMsg;

        let (event_tx, event_rx) = unbounded_channel::<Event>();
        let mut stream = Box::pin(kernel_event_stream(event_rx));

        event_tx
            .send(Event {
                id: "1".to_string(),
                msg: EventMsg::ShutdownComplete,
            })
            .expect("send");

        let first = stream.next().await.expect("first event");
        assert!(matches!(first, Message::KernelEvent(_)));

        drop(event_tx);
        let sentinel = stream.next().await.expect("sentinel before close");
        assert!(matches!(sentinel, Message::KernelDisconnected));
        assert!(stream.next().await.is_none());
    }
}
