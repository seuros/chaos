//! Router scaffolding — minimal typed mailbox primitives for satellite
//! routers in the chaos domain.
//!
//! Naming borrows directly from Thunderbolt / USB4 topology. A **router**
//! is a logical node that owns state and routes inbound traffic. An
//! **adapter** is a typed port on a router. A **packet** is one quantum
//! of traffic across an adapter, optionally carrying a reply channel and
//! a W3C trace `path`. A **tunnel** is a long-lived typed stream
//! (reserved for future use, e.g. SSE or watcher streams).
//!
//! This module is deliberately minimal — it is not an actor framework.
//! Callers construct an mpsc pair, spawn a router task that consumes
//! `Packet<Op>`, and hand the corresponding [`Adapter`] to collaborators.
//! See `chaos-kern`'s rollout recorder for a canonical example.
//!
//! # Invariants
//!
//! - Adapters use **bounded** channels. Full mailboxes must block the
//!   sender; silent drops are disallowed in new router code.
//! - A packet carries at most one `reply` sender. The router must either
//!   consume it or return an error; leaking a oneshot hangs the caller.
//! - The `path` field propagates W3C trace context across the mailbox
//!   hop so OTel spans remain linked through the handoff.

use std::future::Future;

use chaos_ipc::protocol::W3cTraceContext;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

/// Default capacity for router mailboxes. Matches the rollout recorder
/// order of magnitude (256) scaled down for typical router fan-in.
pub const DEFAULT_ADAPTER_CAPACITY: usize = 64;

/// One quantum of inbound traffic to a router.
///
/// `op` is the typed command payload. `reply` is an optional oneshot
/// the router fulfils once the op is processed; omit it for
/// fire-and-forget packets. `path` carries the caller's W3C trace
/// context so the router's span can re-parent to the originating span.
#[derive(Debug)]
pub struct Packet<Op, Reply = ()> {
    pub op: Op,
    pub reply: Option<oneshot::Sender<Reply>>,
    pub path: Option<W3cTraceContext>,
}

impl<Op, Reply> Packet<Op, Reply> {
    /// Fire-and-forget packet with no reply channel.
    pub fn fire(op: Op) -> Self {
        Self {
            op,
            reply: None,
            path: None,
        }
    }

    /// Packet paired with a oneshot reply channel.
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

    /// Attach a W3C trace carrier for propagation across the mailbox hop.
    #[must_use]
    pub fn with_path(mut self, path: Option<W3cTraceContext>) -> Self {
        self.path = path;
        self
    }
}

/// Error returned when a packet cannot be delivered to its router.
///
/// `Closed` means the router is gone (channel dropped). `ReplyDropped`
/// means the router consumed the packet but never fulfilled the reply
/// (a router bug the caller should surface rather than hide).
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

/// Typed send handle for a router.
///
/// Cheap to clone; each clone shares the same underlying mpsc sender.
/// `send` applies backpressure when the mailbox is full and yields
/// until capacity is available — it never drops silently.
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
    /// Wrap an existing sender. Prefer [`Adapter::bounded`] for new routers.
    pub fn new(tx: mpsc::Sender<Packet<Op, Reply>>) -> Self {
        Self { tx }
    }

    /// Build a bounded adapter + receiver pair. The receiver is handed
    /// to the router task; the adapter is cloned to collaborators.
    pub fn bounded(capacity: usize) -> (Self, mpsc::Receiver<Packet<Op, Reply>>) {
        let (tx, rx) = mpsc::channel(capacity);
        (Self { tx }, rx)
    }

    /// Send a fire-and-forget packet. Back-pressures on a full mailbox.
    pub async fn send(&self, op: Op) -> Result<(), AdapterError> {
        self.tx
            .send(Packet::fire(op))
            .await
            .map_err(|_| AdapterError::Closed)
    }

    /// Send a packet and await its typed reply. Back-pressures on a full
    /// mailbox, then waits for the router to fulfil the oneshot.
    pub async fn call(&self, op: Op) -> Result<Reply, AdapterError> {
        let (packet, rx) = Packet::call(op);
        self.tx
            .send(packet)
            .await
            .map_err(|_| AdapterError::Closed)?;
        rx.await.map_err(|_| AdapterError::ReplyDropped)
    }

    /// Send a packet with an explicit W3C trace carrier.
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

    /// Send a request-reply packet with an explicit W3C trace carrier.
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

    /// Current free capacity — useful for health metrics, not for
    /// load shedding (load shedding is explicitly disallowed).
    pub fn capacity(&self) -> usize {
        self.tx.capacity()
    }

    /// `true` if the router task is gone.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// Minimal trait every router implements. Implementations own their
/// state and typically return an [`Adapter`] from a constructor that
/// spawns the router task internally.
///
/// The trait is intentionally narrow. Routers that need more surface
/// (shutdown hooks, health checks, streaming tunnels) expose those
/// as typed ops on their `Op` enum rather than broadening this trait.
pub trait Router {
    /// The command envelope accepted by this router's adapter.
    type Op: Send + 'static;
    /// The reply payload, when the router supports request-reply.
    type Reply: Send + 'static;

    /// Start the router and return an adapter bound to it.
    fn enumerate(self) -> Adapter<Self::Op, Self::Reply>;
}

/// Long-lived typed stream reserved for future use (SSE, watchers,
/// event tunnels). Intentionally unimplemented in this scaffolding —
/// only the type skeleton is exposed so downstream crates can reference
/// the name.
#[allow(dead_code)]
pub struct Tunnel<Op> {
    _phantom: std::marker::PhantomData<fn(Op)>,
}

/// Spawn a router loop with W3C trace context inherited from the
/// caller's current span. The helper instruments the spawned future
/// so spans produced inside the router re-parent to the caller's
/// trace, preserving OTel linkage across the mailbox hop.
pub fn enumerate_traced<F>(span_name: &'static str, fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    use tracing::Instrument;
    let span = tracing::info_span!("router.enumerate", name = span_name);
    tokio::spawn(fut.instrument(span))
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
