//! Console-side adapter around [`chaos_session::ClientSession`].
//!
//! Every function here is a thin bridge: it spawns (or attaches to) a kernel
//! session via the shared `chaos-session` crate, then forwards the resulting
//! event stream into the console's [`AppEventSender`] as
//! [`AppEvent::ChaosEvent`]. No business logic lives in this file — the
//! canonical SQ/EQ lifecycle is owned by `chaos-session` and shared with
//! every other chaos frontend (GUI, headless, ...).

use std::sync::Arc;

use chaos_ipc::protocol::EventMsg;
use chaos_kern::Process;
use chaos_kern::ProcessTable;
use chaos_kern::config::Config;
use chaos_session::ClientSession;
use chaos_session::OpForwarder;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;

const TUI_NOTIFY_CLIENT: &str = "chaos-console";

/// Spawn a fresh kernel session and forward its event stream into the
/// console's [`AppEventSender`].
///
/// Returns the [`Op`] sender used by the UI to submit user actions. The
/// first event delivered to `app_event_tx` will be either
/// [`EventMsg::SessionConfigured`] on a successful boot, or
/// [`EventMsg::Error`] followed by [`AppEvent::FatalExitRequest`] if the
/// kernel failed to start.
pub(crate) fn spawn_agent(
    config: Config,
    app_event_tx: AppEventSender,
    server: Arc<ProcessTable>,
) -> OpForwarder {
    let session = ClientSession::spawn(config, server, Some(TUI_NOTIFY_CLIENT.to_string()));
    let ClientSession { op_tx, event_rx } = session;
    spawn_event_bridge(event_rx, app_event_tx, BridgeMode::EmitFatalOnBootError);
    // Session-backed path: the ClientSession's own DropGuard already
    // manages the drain task, so wrap in a no-op OpForwarder for type
    // uniformity with the #18 forwarder path.
    OpForwarder::from_sender(op_tx)
}

/// Attach to an existing kernel [`Process`] (e.g. after a fork or session
/// resume) and forward its event stream into the console's [`AppEventSender`].
///
/// The caller supplies the captured [`SessionConfiguredEvent`] that the
/// kernel emitted at boot time; `chaos-session` replays it as the first
/// event before pumping subsequent events.
pub(crate) fn spawn_agent_from_existing(
    thread: Arc<Process>,
    session_configured: chaos_ipc::protocol::SessionConfiguredEvent,
    app_event_tx: AppEventSender,
) -> OpForwarder {
    let session = ClientSession::attach(
        thread,
        session_configured,
        Some(TUI_NOTIFY_CLIENT.to_string()),
    );
    let ClientSession { op_tx, event_rx } = session;
    spawn_event_bridge(event_rx, app_event_tx, BridgeMode::Quiet);
    OpForwarder::from_sender(op_tx)
}

/// Spawn an op-forwarding loop for an existing [`Process`] without
/// subscribing to its event stream.
///
/// Used by callers that already own the event stream (for example, a
/// forked process whose events are drained by another task). The returned
/// [`OpForwarder`] owns a drop guard on the drain task's cancellation
/// token — dropping it (or replacing the [`crate::chatwidget::ChatWidget`]
/// that holds it during process switches) tears down the drain task and
/// releases the kernel `Arc<Process>`.
pub(crate) fn spawn_op_forwarder(thread: Arc<Process>) -> OpForwarder {
    chaos_session::spawn_op_forwarder(thread, Some(TUI_NOTIFY_CLIENT.to_string()))
}

/// How the event bridge should handle a boot-time [`EventMsg::Error`] from
/// the kernel.
#[derive(Clone, Copy)]
enum BridgeMode {
    /// If the first event is an Error (i.e. `ProcessTable::start_process`
    /// failed), forward it to the UI and additionally emit a
    /// [`AppEvent::FatalExitRequest`]. Used for cold starts.
    EmitFatalOnBootError,
    /// Forward events as-is and never synthesise a fatal exit request. Used
    /// for warm attaches where the kernel process is already alive.
    Quiet,
}

/// Spawn the background task that drains a `ClientSession` event receiver
/// into the console's [`AppEventSender`], applying the given [`BridgeMode`]
/// for boot-time errors.
fn spawn_event_bridge(
    mut event_rx: UnboundedReceiver<chaos_ipc::protocol::Event>,
    app_event_tx: AppEventSender,
    mode: BridgeMode,
) {
    tokio::spawn(async move {
        let mut seen_first = false;
        while let Some(event) = event_rx.recv().await {
            let is_boot_error = !seen_first && matches!(event.msg, EventMsg::Error(_));
            seen_first = true;

            if is_boot_error && matches!(mode, BridgeMode::EmitFatalOnBootError) {
                // Capture a human-readable summary before moving the event.
                let message = match &event.msg {
                    EventMsg::Error(err) => format!("Failed to initialize chaos: {}", err.message),
                    _ => "Failed to initialize chaos".to_string(),
                };
                tracing::error!("{message}");
                app_event_tx.send(AppEvent::ChaosEvent(event));
                app_event_tx.send(AppEvent::FatalExitRequest(message));
                return;
            }

            let is_shutdown = matches!(event.msg, EventMsg::ShutdownComplete);
            app_event_tx.send(AppEvent::ChaosEvent(event));
            if is_shutdown {
                break;
            }
        }
    });
}
