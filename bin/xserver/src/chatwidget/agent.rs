use std::sync::Arc;

use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_kern::Process;
use chaos_kern::ProcessTable;
use chaos_kern::config::Config;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;

const TUI_NOTIFY_CLIENT: &str = "codex-tui";

async fn initialize_app_server_client_name(thread: &Process) {
    if let Err(err) = thread
        .set_app_server_client_name(Some(TUI_NOTIFY_CLIENT.to_string()))
        .await
    {
        tracing::error!("failed to set app server client name: {err}");
    }
}

/// Spawn the agent bootstrapper and op forwarding loop, returning the
/// `UnboundedSender<Op>` used by the UI to submit operations.
pub(crate) fn spawn_agent(
    config: Config,
    app_event_tx: AppEventSender,
    server: Arc<ProcessTable>,
) -> UnboundedSender<Op> {
    let (codex_op_tx, mut codex_op_rx) = unbounded_channel::<Op>();

    let app_event_tx_clone = app_event_tx;
    tokio::spawn(async move {
        let new_process = match server.start_process(config).await {
            Ok(v) => v,
            Err(err) => {
                let message = format!("Failed to initialize codex: {err}");
                tracing::error!("{message}");
                app_event_tx_clone.send(AppEvent::CodexEvent(Event {
                    id: "".to_string(),
                    msg: EventMsg::Error(err.to_error_event(/*message_prefix*/ None)),
                }));
                app_event_tx_clone.send(AppEvent::FatalExitRequest(message));
                tracing::error!("failed to initialize codex: {err}");
                return;
            }
        };
        let (_, thread, session_configured) = new_process.into_parts();
        initialize_app_server_client_name(thread.as_ref()).await;

        // Forward the captured `SessionConfigured` event so it can be rendered in the UI.
        let ev = chaos_ipc::protocol::Event {
            // The `id` does not matter for rendering, so we can use a fake value.
            id: "".to_string(),
            msg: chaos_ipc::protocol::EventMsg::SessionConfigured(session_configured),
        };
        app_event_tx_clone.send(AppEvent::CodexEvent(ev));

        let process_clone = thread.clone();
        tokio::spawn(async move {
            while let Some(op) = codex_op_rx.recv().await {
                let id = process_clone.submit(op).await;
                if let Err(e) = id {
                    tracing::error!("failed to submit op: {e}");
                }
            }
        });

        while let Ok(event) = thread.next_event().await {
            let is_shutdown_complete = matches!(event.msg, EventMsg::ShutdownComplete);
            app_event_tx_clone.send(AppEvent::CodexEvent(event));
            if is_shutdown_complete {
                // ShutdownComplete is terminal for a thread; drop this receiver task so
                // the Arc<Process> can be released and thread resources can clean up.
                break;
            }
        }
    });

    codex_op_tx
}

/// Spawn agent loops for an existing process (e.g., a forked process).
/// Sends the provided `SessionConfiguredEvent` immediately, then forwards subsequent
/// events and accepts Ops for submission.
pub(crate) fn spawn_agent_from_existing(
    thread: std::sync::Arc<Process>,
    session_configured: chaos_ipc::protocol::SessionConfiguredEvent,
    app_event_tx: AppEventSender,
) -> UnboundedSender<Op> {
    let (codex_op_tx, mut codex_op_rx) = unbounded_channel::<Op>();

    let app_event_tx_clone = app_event_tx;
    tokio::spawn(async move {
        initialize_app_server_client_name(thread.as_ref()).await;

        // Forward the captured `SessionConfigured` event so it can be rendered in the UI.
        let ev = chaos_ipc::protocol::Event {
            id: "".to_string(),
            msg: chaos_ipc::protocol::EventMsg::SessionConfigured(session_configured),
        };
        app_event_tx_clone.send(AppEvent::CodexEvent(ev));

        let process_clone = thread.clone();
        tokio::spawn(async move {
            while let Some(op) = codex_op_rx.recv().await {
                let id = process_clone.submit(op).await;
                if let Err(e) = id {
                    tracing::error!("failed to submit op: {e}");
                }
            }
        });

        while let Ok(event) = thread.next_event().await {
            let is_shutdown_complete = matches!(event.msg, EventMsg::ShutdownComplete);
            app_event_tx_clone.send(AppEvent::CodexEvent(event));
            if is_shutdown_complete {
                // ShutdownComplete is terminal for a thread; drop this receiver task so
                // the Arc<Process> can be released and thread resources can clean up.
                break;
            }
        }
    });

    codex_op_tx
}

/// Spawn an op-forwarding loop for an existing process without subscribing to events.
pub(crate) fn spawn_op_forwarder(thread: std::sync::Arc<Process>) -> UnboundedSender<Op> {
    let (codex_op_tx, mut codex_op_rx) = unbounded_channel::<Op>();

    tokio::spawn(async move {
        initialize_app_server_client_name(thread.as_ref()).await;
        while let Some(op) = codex_op_rx.recv().await {
            if let Err(e) = thread.submit(op).await {
                tracing::error!("failed to submit op: {e}");
            }
        }
    });

    codex_op_tx
}
