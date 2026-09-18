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
    let mut send = tokio_test::task::spawn(adapter.send(PingOp::Ping));
    tokio_test::assert_pending!(send.poll());
    let _ = rx.recv().await.expect("drain");
    assert!(send.is_woken(), "draining capacity must wake the sender");
    tokio_test::assert_ready!(send.poll()).expect("unblocked send");
    assert!(matches!(
        rx.try_recv().expect("second packet").op,
        PingOp::Ping
    ));
}
