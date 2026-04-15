//! Shared test helpers and fixtures.

use tokio::sync::mpsc::UnboundedReceiver;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;

/// Create an AppEventSender for tests. The receiver is dropped.
pub(crate) fn make_app_event_sender() -> AppEventSender {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    AppEventSender::new(tx)
}

/// Create an AppEventSender for tests along with the receiver for asserting events.
pub(crate) fn make_app_event_sender_with_rx() -> (AppEventSender, UnboundedReceiver<AppEvent>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
    (AppEventSender::new(tx), rx)
}
