use super::*;

type Decision = oneshot::Sender<protocol::Result<RequestPermissionResponse>>;

#[derive(Debug)]
struct ControlledPermissions(mpsc::UnboundedSender<(SessionId, Decision)>);

impl PermissionHandler for ControlledPermissions {
    fn request_permission(
        &self,
        request: RequestPermissionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<RequestPermissionResponse>> {
        let (decision, pending) = oneshot::channel();
        self.0
            .send((request.session_id, decision))
            .expect("policy observer");
        Box::pin(async move { pending.await.expect("application decision") })
    }
}

fn allow() -> RequestPermissionResponse {
    serde_json::from_value(json!({
        "outcome":{"outcome":"selected","optionId":"allow"}
    }))
    .expect("permission response")
}

async fn ask(peer: &mut Peer, id: &str, session: &str) {
    peer.send(json!({
        "jsonrpc":"2.0","id":id,"method":"session/request_permission",
        "params":{
            "sessionId":session,"toolCall":{"toolCallId":id},
            "options":[{"optionId":"allow","name":"Allow","kind":"allow_once"}]
        }
    }))
    .await;
}

#[tokio::test]
async fn cancellation_resolves_all_pending_permissions_and_is_session_scoped() {
    let (decisions, mut pending) = mpsc::unbounded_channel();
    let (connection, _updates, mut peer) = connect(ConnectionOptions {
        permissions: Arc::new(ControlledPermissions(decisions)),
        ..Default::default()
    })
    .await;
    let mut cancelled = Vec::new();
    for id in ["p1", "p2"] {
        ask(&mut peer, id, "s1").await;
        let (session, decision) = pending.recv().await.expect("pending permission");
        assert_eq!(session, SessionId::new("s1"));
        cancelled.push(decision);
    }
    ask(&mut peer, "other", "s2").await;
    let (_, other) = pending.recv().await.expect("other session permission");
    connection
        .client()
        .notify(CancelNotification::new("s1"))
        .expect("cancel");
    let mut replies = Vec::new();
    for _ in 0..3 {
        let frame = peer.receive().await.expect("cancel or permission response");
        if frame["method"] == "session/cancel" {
            assert_eq!(frame["params"]["sessionId"], "s1");
        } else {
            assert_eq!(frame["result"]["outcome"]["outcome"], "cancelled");
            replies.push(frame["id"].as_str().expect("permission ID").to_owned());
        }
    }
    replies.sort();
    assert_eq!(replies, ["p1", "p2"]);
    for decision in cancelled {
        assert!(
            decision.send(Ok(allow())).is_err(),
            "late approval cannot escape"
        );
    }
    other
        .send(Ok(allow()))
        .expect("unrelated policy still active");
    let reply = peer.receive().await.expect("other response");
    assert_eq!(reply["id"], "other");
    assert_eq!(reply["result"]["outcome"]["outcome"], "selected");

    // Cancellation must not poison future permissions in a subsequent turn.
    ask(&mut peer, "fresh", "s1").await;
    let (_, fresh) = pending.recv().await.expect("fresh permission");
    fresh.send(Ok(allow())).expect("fresh decision");
    let reply = peer.receive().await.expect("fresh response");
    assert_eq!(reply["id"], "fresh");
    assert_eq!(reply["result"]["outcome"]["outcome"], "selected");
    connection.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn ready_application_approval_loses_to_cancellation_before_response_submission() {
    let (decisions, mut pending) = mpsc::unbounded_channel();
    let (connection, _updates, mut peer) = connect(ConnectionOptions {
        permissions: Arc::new(ControlledPermissions(decisions)),
        ..Default::default()
    })
    .await;
    ask(&mut peer, "race", "s1").await;
    let (_, decision) = pending.recv().await.expect("pending permission");
    // This current-thread test makes both branches ready before the handler
    // runs again. The cancellation branch must win even for a ready approval.
    decision.send(Ok(allow())).expect("ready approval");
    connection
        .client()
        .notify(CancelNotification::new("s1"))
        .expect("cancel");
    for _ in 0..2 {
        let frame = peer.receive().await.expect("cancel or permission response");
        if frame["id"] == "race" {
            assert_eq!(frame["result"]["outcome"]["outcome"], "cancelled");
        } else {
            assert_eq!(frame["method"], "session/cancel");
        }
    }
    connection.disconnect().await.expect("disconnect");
}
