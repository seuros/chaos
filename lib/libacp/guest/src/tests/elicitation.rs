use super::*;
use protocol::schema::v1::*;

type Decision = oneshot::Sender<protocol::Result<CreateElicitationResponse>>;
#[derive(Debug)]
struct Ui(mpsc::UnboundedSender<(CreateElicitationRequest, Decision)>);
impl ElicitationHandler for Ui {
    fn capabilities(&self) -> ElicitationCapabilities {
        ElicitationCapabilities::new().form(ElicitationFormCapabilities::new())
    }
    fn create(
        &self,
        request: CreateElicitationRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<CreateElicitationResponse>> {
        let (decision, pending) = oneshot::channel();
        self.0.send((request, decision)).unwrap();
        Box::pin(async move { pending.await.unwrap() })
    }
}

async fn form(peer: &mut Peer, id: &str, session: &str) {
    peer.send(
        json!({"jsonrpc":"2.0","id":id,"method":"elicitation/create","params":{
            "mode":"form","sessionId":session,"message":"Choose","requestedSchema":{
                "type":"object","properties":{"choice":{"type":"string"}},"required":["choice"]
            }
        }}),
    )
    .await;
}

#[tokio::test]
async fn form_ui_is_opt_in_mode_checked_and_session_cancellation_wins_over_acceptance() {
    let (decisions, mut pending) = mpsc::unbounded_channel();
    let (connection, _, mut peer) = connect(ConnectionOptions {
        elicitation: Some(Arc::new(Ui(decisions))),
        ..Default::default()
    })
    .await;
    form(&mut peer, "first", "s1").await;
    let (request, first) = pending.recv().await.unwrap();
    assert_eq!(request.message, "Choose");
    form(&mut peer, "other", "s2").await;
    let (_, other) = pending.recv().await.unwrap();
    connection
        .client()
        .notify(CancelNotification::new("s1"))
        .unwrap();
    let mut cancelled = false;
    for _ in 0..2 {
        let reply = peer.receive().await.unwrap();
        if reply["id"] == "first" {
            assert_eq!(reply["result"]["action"], "cancel");
            cancelled = true;
        }
    }
    assert!(cancelled);
    assert!(first.is_closed());
    other
        .send(Ok(CreateElicitationResponse::new(
            ElicitationAction::Decline,
        )))
        .unwrap();
    assert_eq!(peer.receive().await.unwrap()["result"]["action"], "decline");
    peer.send(json!({"jsonrpc":"2.0","id":"url","method":"elicitation/create","params":{
        "mode":"url","sessionId":"s1","message":"Login","url":"https://example.invalid","elicitationId":"e"
    }})).await;
    assert!(peer.receive().await.unwrap()["error"].is_object());
    assert!(
        pending.try_recv().is_err(),
        "unnegotiated URL is never opened"
    );
    connection.disconnect().await.unwrap();
}

#[tokio::test]
async fn form_acceptance_is_typed_and_timeout_is_cancel_not_accept() {
    let (decisions, mut pending) = mpsc::unbounded_channel();
    let (connection, _, mut peer) = connect(ConnectionOptions {
        elicitation: Some(Arc::new(Ui(decisions))),
        request_timeout: Duration::from_millis(100),
        ..Default::default()
    })
    .await;
    form(&mut peer, "accept", "s").await;
    let (_, decide) = pending.recv().await.unwrap();
    decide
        .send(Ok(serde_json::from_value(
            json!({"action":"accept","content":{"choice":"yes"}}),
        )
        .unwrap()))
        .unwrap();
    assert_eq!(
        peer.receive().await.unwrap()["result"]["content"]["choice"],
        "yes"
    );
    form(&mut peer, "expire", "s").await;
    let (_, late) = pending.recv().await.unwrap();
    assert_eq!(peer.receive().await.unwrap()["result"]["action"], "cancel");
    assert!(late.is_closed());
    connection.disconnect().await.unwrap();
}

#[derive(Debug)]
struct UrlUi(mpsc::UnboundedSender<String>);
impl ElicitationHandler for UrlUi {
    fn capabilities(&self) -> ElicitationCapabilities {
        ElicitationCapabilities::new().url(ElicitationUrlCapabilities::new())
    }
    fn create(
        &self,
        request: CreateElicitationRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<CreateElicitationResponse>> {
        assert!(
            matches!(request.scope(), ElicitationScope::Request(scope) if serde_json::to_value(&scope.request_id).unwrap() == json!("auth"))
        );
        let ElicitationMode::Url(url) = request.mode else {
            panic!("URL mode")
        };
        self.0.send(url.url).unwrap();
        Box::pin(async {
            Ok(CreateElicitationResponse::new(ElicitationAction::Accept(
                ElicitationAcceptAction::new(),
            )))
        })
    }
    fn complete(
        &self,
        notification: CompleteElicitationNotification,
    ) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        self.0
            .send(notification.elicitation_id.to_string())
            .unwrap();
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn url_request_scope_and_completion_dispatch_without_opening_a_browser() {
    let (events, mut observed) = mpsc::unbounded_channel();
    let (connection, _, mut peer) = connect(ConnectionOptions {
        elicitation: Some(Arc::new(UrlUi(events))),
        ..Default::default()
    })
    .await;
    peer.send(json!({"jsonrpc":"2.0","id":"url","method":"elicitation/create","params":{
        "mode":"url","requestId":"auth","message":"Login","url":"https://example.invalid/consent","elicitationId":"e"
    }})).await;
    assert_eq!(
        observed.recv().await.unwrap(),
        "https://example.invalid/consent"
    );
    let result = peer.receive().await.unwrap();
    assert_eq!(result["result"]["action"], "accept");
    assert!(result["result"].get("content").is_none());
    peer.send(json!({"jsonrpc":"2.0","id":"duplicate","method":"elicitation/create","params":{
        "mode":"url","requestId":"auth","message":"Duplicate","url":"https://example.invalid/consent","elicitationId":"e"
    }})).await;
    assert_eq!(peer.receive().await.unwrap()["error"]["code"], -32602);
    peer.send(
        json!({"jsonrpc":"2.0","method":"elicitation/complete","params":{"elicitationId":"e"}}),
    )
    .await;
    assert_eq!(observed.recv().await.unwrap(), "e");
    for id in ["e", "unknown"] {
        peer.send(
            json!({"jsonrpc":"2.0","method":"elicitation/complete","params":{"elicitationId":id}}),
        )
        .await;
    }
    peer.send(json!({"jsonrpc":"2.0","id":"barrier","method":"fs/read_text_file","params":{"sessionId":"s","path":"/unavailable"}})).await;
    assert_eq!(peer.receive().await.unwrap()["id"], "barrier");
    assert!(
        observed.try_recv().is_err(),
        "unknown and already-completed IDs must be ignored"
    );
    connection.disconnect().await.unwrap();
}
