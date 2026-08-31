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
mod tests {
    use super::*;

    #[derive(Debug)]
    enum PingOp {
        Ping,
    }

    #[tokio::test]
    async fn fire_and_forget_round_trip() {
        let (adapter, mut rx) = Adapter::<PingOp>::bounded(4);
        adapter.send(PingOp::Ping).await.expect("send");
        let pkt = rx.recv().await.expect("recv");
        assert!(matches!(pkt.op, PingOp::Ping));
        assert!(pkt.reply.is_none());
        assert!(pkt.path.is_none());
    }

    #[tokio::test]
    async fn call_awaits_reply() {
        let (adapter, mut rx) = Adapter::<PingOp, u32>::bounded(4);
        let server = tokio::spawn(async move {
            let pkt = rx.recv().await.expect("recv");
            let reply = pkt.reply.expect("reply sender");
            reply.send(47).expect("send reply");
        });
        let got = adapter.call(PingOp::Ping).await.expect("call");
        assert_eq!(got, 47);
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn call_traced_carries_path() {
        let (adapter, mut rx) = Adapter::<PingOp, ()>::bounded(4);
        let carrier = W3cTraceContext {
            traceparent: Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".into()),
            tracestate: None,
        };
        let expected = carrier.clone();
        let server = tokio::spawn(async move {
            let pkt = rx.recv().await.expect("recv");
            assert_eq!(pkt.path, Some(expected));
            pkt.reply.expect("reply").send(()).expect("send reply");
        });
        adapter
            .call_traced(PingOp::Ping, Some(carrier))
            .await
            .expect("call_traced");
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn closed_router_returns_closed_error() {
        let (adapter, rx) = Adapter::<PingOp>::bounded(4);
        drop(rx);
        let err = adapter.send(PingOp::Ping).await.unwrap_err();
        assert!(matches!(err, AdapterError::Closed));
    }

    #[tokio::test]
    async fn dropped_reply_sender_surfaces_error() {
        let (adapter, mut rx) = Adapter::<PingOp, u32>::bounded(4);
        let server = tokio::spawn(async move {
            let pkt = rx.recv().await.expect("recv");
            drop(pkt.reply);
        });
        let err = adapter.call(PingOp::Ping).await.unwrap_err();
        assert!(matches!(err, AdapterError::ReplyDropped));
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn bounded_channel_backpressures_rather_than_drops() {
        let (adapter, mut rx) = Adapter::<PingOp>::bounded(1);
        adapter.send(PingOp::Ping).await.expect("first send");
        // Second send blocks until receiver drains the first.
        let send_fut = adapter.send(PingOp::Ping);
        tokio::pin!(send_fut);
        tokio::select! {
            _ = &mut send_fut => panic!("second send should back-pressure"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
        }
        let _ = rx.recv().await.expect("drain");
        send_fut.await.expect("unblocked send");
    }
}
