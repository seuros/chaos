use super::*;
use protocol::schema::v1::{
    CreateElicitationRequest, CreateElicitationResponse, ElicitationAction,
    ElicitationCapabilities, ElicitationFormCapabilities, ElicitationFormMode,
    ElicitationSessionScope,
};
use std::assert_matches;

pub(super) async fn ask(context: &SessionContext) -> protocol::Result<()> {
    let schema = serde_json::from_value(serde_json::json!({
        "type":"object","properties":{"choice":{"type":"string"}},"required":["choice"]
    }))
    .unwrap();
    let response = context
        .client()
        .create_elicitation(CreateElicitationRequest::new(
            ElicitationFormMode::new(
                ElicitationSessionScope::new(context.session_id().clone()),
                schema,
            ),
            "Choose",
        ))
        .await?;
    context.client().update(message(match response.action {
        ElicitationAction::Accept(_) => "accepted",
        ElicitationAction::Decline => "declined",
        _ => "cancelled",
    }))?;
    Ok(())
}

#[derive(Debug)]
struct Ui {
    pending: tokio::sync::Notify,
    respond: bool,
}
impl acp_guest::ElicitationHandler for Ui {
    fn capabilities(&self) -> ElicitationCapabilities {
        ElicitationCapabilities::new().form(ElicitationFormCapabilities::new())
    }
    fn create(
        &self,
        request: CreateElicitationRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<CreateElicitationResponse>> {
        assert_eq!(request.message, "Choose");
        Box::pin(async move {
            self.pending.notify_one();
            if !self.respond {
                std::future::pending::<()>().await;
            }
            Ok(CreateElicitationResponse::new(ElicitationAction::Decline))
        })
    }
}

#[tokio::test]
async fn negotiated_elicitation_round_trip_and_cancellation_are_turn_scoped() {
    for respond in [true, false] {
        let (stream, task, _) = serve(ConnectionLimits::default(), 65536);
        let ui = Arc::new(Ui {
            pending: tokio::sync::Notify::new(),
            respond,
        });
        let (reader, writer) = tokio::io::split(stream);
        let (connection, mut updates) = AcpConnection::connect(
            reader,
            writer,
            ConnectionOptions {
                elicitation: Some(ui.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let id = session(&connection).await;
        let client = connection.client();
        let pending = {
            let client = client.clone();
            let id = id.clone();
            tokio::spawn(async move { client.request(prompt(&id, "elicit")).await })
        };
        ui.pending.notified().await;
        if !respond {
            client.notify(CancelNotification::new(id.clone())).unwrap();
        }
        let result = pending.await.unwrap().unwrap();
        assert_eq!(
            result.stop_reason,
            if respond {
                StopReason::EndTurn
            } else {
                StopReason::Cancelled
            }
        );
        let update = serde_json::to_value(updates.recv().await.unwrap()).unwrap();
        assert_eq!(
            update["update"]["content"]["text"],
            if respond { "declined" } else { "cancelled" }
        );
        assert_eq!(
            client
                .request(prompt(&id, "fresh"))
                .await
                .unwrap()
                .stop_reason,
            StopReason::EndTurn
        );
        connection.disconnect().await.unwrap();
        task.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn absent_elicitation_capability_never_dispatches_a_ui_request() {
    let (connection, _updates, task, _) = start(ConnectionLimits::default()).await;
    let id = session(&connection).await;
    assert!(
        connection
            .client()
            .request(prompt(&id, "elicit"))
            .await
            .is_err()
    );
    connection.disconnect().await.unwrap();
    task.await.unwrap().unwrap();
}

#[derive(Debug)]
struct UrlUi(mpsc::UnboundedSender<String>);
impl acp_guest::ElicitationHandler for UrlUi {
    fn capabilities(&self) -> ElicitationCapabilities {
        ElicitationCapabilities::new().url(protocol::schema::v1::ElicitationUrlCapabilities::new())
    }
    fn create(
        &self,
        _: CreateElicitationRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<CreateElicitationResponse>> {
        Box::pin(async {
            Ok(CreateElicitationResponse::new(ElicitationAction::Accept(
                protocol::schema::v1::ElicitationAcceptAction::new(),
            )))
        })
    }
    fn complete(
        &self,
        notification: protocol::schema::v1::CompleteElicitationNotification,
    ) -> protocol::BoxFuture<'_, protocol::Result<()>> {
        self.0
            .send(notification.elicitation_id.to_string())
            .unwrap();
        Box::pin(async { Ok(()) })
    }
}

fn url_request(id: &SessionId, url_id: &str) -> CreateElicitationRequest {
    CreateElicitationRequest::new(
        protocol::schema::v1::ElicitationUrlMode::new(
            ElicitationSessionScope::new(id.clone()),
            url_id.to_string(),
            "https://example.invalid",
        ),
        "Consent",
    )
}

#[tokio::test]
async fn url_ids_are_connection_bound_unique_and_completed_only_once() {
    let (stream, task, observations) = serve(ConnectionLimits::default(), 65536);
    let (completed, mut observed) = mpsc::unbounded_channel();
    let (reader, writer) = tokio::io::split(stream);
    let (connection, mut updates) = AcpConnection::connect(
        reader,
        writer,
        ConnectionOptions {
            elicitation: Some(Arc::new(UrlUi(completed))),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let id = session(&connection).await;
    let client = connection.client();
    let pending = {
        let client = client.clone();
        let id = id.clone();
        tokio::spawn(async move { client.request(prompt(&id, "wait")).await })
    };
    updates.recv().await.unwrap();
    let context = observations.context.lock().unwrap().clone().unwrap();
    let request = url_request(&id, "url-id");
    assert_matches!(
        context
            .client()
            .create_elicitation(request.clone())
            .await
            .unwrap()
            .action,
        ElicitationAction::Accept(_)
    );
    assert!(context.client().create_elicitation(request).await.is_err());
    assert!(context.client().complete_elicitation("unknown").is_err());
    context.client().complete_elicitation("url-id").unwrap();
    assert_eq!(observed.recv().await.unwrap(), "url-id");
    assert!(context.client().complete_elicitation("url-id").is_err());
    let foreign = url_request(&SessionId::new("foreign"), "foreign-id");
    assert!(context.client().create_elicitation(foreign).await.is_err());
    assert_matches!(
        context
            .client()
            .create_elicitation(url_request(&id, "late"))
            .await
            .unwrap()
            .action,
        ElicitationAction::Accept(_)
    );
    assert_matches!(
        context
            .client()
            .create_elicitation(url_request(&id, "on-close"))
            .await
            .unwrap()
            .action,
        ElicitationAction::Accept(_)
    );
    client.notify(CancelNotification::new(id.clone())).unwrap();
    assert_eq!(
        pending.await.unwrap().unwrap().stop_reason,
        StopReason::Cancelled
    );
    assert!(context.client().capabilities().is_err());
    let other = session(&connection).await;
    client.request(prompt(&other, "fresh")).await.unwrap();
    let foreign = observations.context.lock().unwrap().clone().unwrap();
    assert!(foreign.client().complete_elicitation("late").is_err());
    // The originating turn is over, but the URL still belongs to its session.
    context.client().complete_elicitation("late").unwrap();
    assert_eq!(observed.recv().await.unwrap(), "late");
    assert!(context.client().complete_elicitation("late").is_err());
    client
        .request(protocol::schema::v1::CloseSessionRequest::new(id))
        .await
        .unwrap();
    assert!(context.client().complete_elicitation("on-close").is_err());
    connection.disconnect().await.unwrap();
    task.await.unwrap().unwrap();
    assert!(context.client().complete_elicitation("late").is_err());
}

#[tokio::test]
async fn unsuccessful_url_requests_release_their_reservations() {
    use serde_json::json;

    for failure in [
        "error",
        "decline",
        "cancel",
        "content",
        "malformed",
        "timeout",
        "drop",
    ] {
        let (mut peer, task, observations) = faults::raw(ConnectionLimits {
            approval_timeout: Duration::from_millis(100),
            pending_requests: 4,
            ..Default::default()
        });
        peer.send(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":1,"clientCapabilities":{"elicitation":{"url":{}}}
            }}),
        )
        .await;
        assert!(peer.receive().await["error"].is_null());
        peer.send(json!({"jsonrpc":"2.0","id":2,"method":"session/new",
            "params":{"cwd":"/workspace","mcpServers":[]}}))
            .await;
        let id = peer.receive().await["result"]["sessionId"].clone();
        peer.prompt(3, &id, "wait").await;
        assert_eq!(peer.receive().await["method"], "session/update");
        let context = observations.context.lock().unwrap().clone().unwrap();
        let request = url_request(context.session_id(), "retry");
        let pending = {
            let context = context.clone();
            let request = request.clone();
            tokio::spawn(async move { context.client().create_elicitation(request).await })
        };
        let frame = peer.receive().await;
        assert_eq!(frame["method"], "elicitation/create", "{failure}");
        match failure {
            "timeout" => {
                assert_matches!(
                    pending.await.unwrap().unwrap().action,
                    ElicitationAction::Cancel
                );
            }
            "drop" => {
                pending.abort();
                assert!(pending.await.unwrap_err().is_cancelled());
            }
            _ => {
                let reply = match failure {
                    "error" => json!({"error":{"code":-32603,"message":"UI failed"}}),
                    "content" => json!({"result":{"action":"accept","content":{"bad":true}}}),
                    "malformed" => json!({"result":{"action":"invalid"}}),
                    action => json!({"result":{"action":action}}),
                };
                let mut reply = reply;
                reply["jsonrpc"] = json!("2.0");
                reply["id"] = frame["id"].clone();
                peer.send(reply).await;
                assert!(!matches!(
                    pending.await.unwrap().unwrap().action,
                    ElicitationAction::Accept(_)
                ));
            }
        }
        if matches!(failure, "timeout" | "drop") {
            // Settle the wire request too; its admission is independent of URL IDs.
            peer.send(json!({"jsonrpc":"2.0","id":frame["id"],"result":{"action":"cancel"}}))
                .await;
            // An ordered round trip ensures the late reply was accounted for.
            peer.send(json!({"jsonrpc":"2.0","id":4,"method":"session/new",
                "params":{"cwd":"/workspace","mcpServers":[]}}))
                .await;
            assert!(peer.receive().await["error"].is_null());
        }
        let retry = tokio::spawn(async move { context.client().create_elicitation(request).await });
        let frame = peer.receive().await;
        assert_eq!(frame["method"], "elicitation/create", "{failure}");
        peer.send(json!({"jsonrpc":"2.0","id":frame["id"],"result":{"action":"accept"}}))
            .await;
        assert_matches!(
            retry.await.unwrap().unwrap().action,
            ElicitationAction::Accept(_)
        );
        drop(peer);
        let _ = task.await.unwrap();
    }
}
