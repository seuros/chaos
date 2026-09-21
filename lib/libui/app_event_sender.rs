use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc::UnboundedSender;

use crate::app_event::AppEvent;
use crate::app_event::UiCommand;
use crate::session_log;

#[derive(Clone, Debug)]
pub struct AppEventSender {
    pub app_event_tx: UnboundedSender<AppEvent>,
    current_view: Arc<AtomicU64>,
    view: Option<u64>,
}

impl AppEventSender {
    pub fn new(app_event_tx: UnboundedSender<AppEvent>) -> Self {
        Self {
            app_event_tx,
            current_view: Arc::new(AtomicU64::new(0)),
            view: None,
        }
    }

    /// Invalidate older widget work without replacing the process event channel.
    pub fn for_new_view(&self) -> Self {
        Self {
            app_event_tx: self.app_event_tx.clone(),
            current_view: Arc::clone(&self.current_view),
            view: Some(self.current_view.fetch_add(1, Ordering::Relaxed) + 1),
        }
    }

    /// Send an event to the app event channel. If it fails, we swallow the
    /// error and log it.
    pub fn send(&self, event: AppEvent) {
        // Record inbound events for high-fidelity session replay.
        // Avoid double-logging Ops; those are logged at the point of submission.
        if !matches!(event, AppEvent::ChaosOp(_)) {
            session_log::log_inbound_app_event(&event);
        }
        let event = match self.view {
            Some(view)
                if !matches!(
                    event,
                    AppEvent::ChaosEvent(_)
                        | AppEvent::ProcessEvent { .. }
                        | AppEvent::ProcessStreamClosed(_)
                        | AppEvent::SubmitProcessOp { .. }
                        | AppEvent::ReloadProjectMcpForProcess(_)
                        | AppEvent::OpenUrlElicitationInBrowser { .. }
                ) =>
            {
                AppEvent::ForView {
                    view,
                    current_view: Arc::clone(&self.current_view),
                    event: Box::new(event),
                }
            }
            _ => event,
        };
        if let Err(e) = self.app_event_tx.send(event) {
            tracing::error!("failed to send event: {e}");
        }
    }

    /// Emit an imperative UI command over the app-event bus.
    pub fn emit_ui_command(&self, command: UiCommand) {
        self.send(AppEvent::UiCommand(command));
    }
}
