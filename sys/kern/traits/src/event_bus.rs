//! Event emission trait — decouples event producers from the concrete Session event channel.

use async_channel::Sender;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;

/// Asynchronous event bus for publishing session events.
///
/// Provides raw channel access for satellite crates that need to emit events without depending
/// on the full Session type. The higher-level `send_event` (with rollout persistence and legacy
/// event fanout) remains in core.
pub trait EventEmitter: Send + Sync {
    /// Raw channel sender for pushing events directly.
    fn event_sender(&self) -> Sender<Event>;

    /// Send an event with a sub-id. The default implementation wraps and sends via the channel.
    fn emit_event(&self, sub_id: String, msg: EventMsg) -> impl Future<Output = ()> + Send {
        let tx = self.event_sender();
        async move {
            let event = Event { id: sub_id, msg };
            let _ = tx.send(event).await;
        }
    }
}

use std::future::Future;
