//! Actor mailboxes for kernel services.

use chaos_ipc::protocol::W3cTraceContext;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

/// Default mailbox capacity.
pub const DEFAULT_ADAPTER_CAPACITY: usize = 64;

/// Actor message.
#[derive(Debug)]
pub struct Packet<Op, Reply = ()> {
    pub op: Op,
    pub reply: Option<oneshot::Sender<Reply>>,
    pub path: Option<W3cTraceContext>,
}

impl<Op, Reply> Packet<Op, Reply> {
    /// Builds a message without a reply.
    pub fn fire(op: Op) -> Self {
        Self {
            op,
            reply: None,
            path: None,
        }
    }

    /// Builds a message with a reply.
    pub fn call(op: Op) -> (Self, oneshot::Receiver<Reply>) {
        let (tx, rx) = oneshot::channel();
        (
            Self {
                op,
                reply: Some(tx),
                path: None,
            },
            rx,
        )
    }

    /// Sets the trace context.
    #[must_use]
    pub fn with_path(mut self, path: Option<W3cTraceContext>) -> Self {
        self.path = path;
        self
    }
}

/// Mailbox error.
#[derive(Debug)]
pub enum AdapterError {
    Closed,
    ReplyDropped,
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterError::Closed => write!(f, "router adapter is closed"),
            AdapterError::ReplyDropped => write!(f, "router dropped reply channel"),
        }
    }
}

impl std::error::Error for AdapterError {}

/// Actor mailbox.
#[derive(Debug)]
pub struct Adapter<Op, Reply = ()> {
    tx: mpsc::Sender<Packet<Op, Reply>>,
}

impl<Op, Reply> Clone for Adapter<Op, Reply> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl<Op, Reply> Adapter<Op, Reply> {
    /// Wraps a sender.
    pub fn new(tx: mpsc::Sender<Packet<Op, Reply>>) -> Self {
        Self { tx }
    }

    /// Builds a bounded mailbox.
    pub fn bounded(capacity: usize) -> (Self, mpsc::Receiver<Packet<Op, Reply>>) {
        let (tx, rx) = mpsc::channel(capacity);
        (Self { tx }, rx)
    }

    /// Sends a message.
    pub async fn send(&self, op: Op) -> Result<(), AdapterError> {
        self.tx
            .send(Packet::fire(op))
            .await
            .map_err(|_| AdapterError::Closed)
    }

    /// Sends a message and waits for its reply.
    pub async fn call(&self, op: Op) -> Result<Reply, AdapterError> {
        let (packet, rx) = Packet::call(op);
        self.tx
            .send(packet)
            .await
            .map_err(|_| AdapterError::Closed)?;
        rx.await.map_err(|_| AdapterError::ReplyDropped)
    }

    /// Sends a traced message.
    pub async fn send_traced(
        &self,
        op: Op,
        path: Option<W3cTraceContext>,
    ) -> Result<(), AdapterError> {
        self.tx
            .send(Packet::fire(op).with_path(path))
            .await
            .map_err(|_| AdapterError::Closed)
    }

    /// Sends a traced message and waits for its reply.
    pub async fn call_traced(
        &self,
        op: Op,
        path: Option<W3cTraceContext>,
    ) -> Result<Reply, AdapterError> {
        let (packet, rx) = Packet::call(op);
        self.tx
            .send(packet.with_path(path))
            .await
            .map_err(|_| AdapterError::Closed)?;
        rx.await.map_err(|_| AdapterError::ReplyDropped)
    }

    /// Returns free mailbox capacity.
    pub fn capacity(&self) -> usize {
        self.tx.capacity()
    }

    /// Returns whether the mailbox is closed.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

#[cfg(test)]
mod tests;
