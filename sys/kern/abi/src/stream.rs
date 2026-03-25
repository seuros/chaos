//! Turn stream — the response channel from model adapters.

use crate::error::AbiError;
use crate::event::TurnEvent;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use tokio::sync::mpsc;

/// A stream of [`TurnEvent`]s from a model adapter.
///
/// Backed by a tokio mpsc channel. Adapters spawn a task that pushes
/// events; the core consumes them via the [`Stream`](futures::Stream) impl.
pub struct TurnStream {
    pub rx_event: mpsc::Receiver<Result<TurnEvent, AbiError>>,
}

impl futures::Stream for TurnStream {
    type Item = Result<TurnEvent, AbiError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx_event.poll_recv(cx)
    }
}
