use super::*;
use anyhow::{Context, Result};
use mcp_host::protocol::types::{JsonRpcRequest, JsonRpcResponse};
use mcp_host::server::multiplexer::{JsonRpcClientRequest, RequestMultiplexer};
use serde_json::json;
use tokio::sync::mpsc;

struct Fixture {
    server: Arc<Server>,
    outgoing: OutgoingMessageSender,
    requests: mpsc::Receiver<JsonRpcClientRequest>,
    multiplexer: Arc<RequestMultiplexer>,
    urls: Arc<UrlElicitations>,
}

#[tokio::test]
async fn session_close_cancels_own_pending_request_without_touching_other_sessions() -> Result<()> {
    let mut f = fixture("2025-11-25", json!({"elicitation": {}})).await?;
    let (closed, lifetime) = watch::channel(false);
    f.outgoing = f.outgoing.with_lifetime(lifetime);
    let receiver = f.outgoing.request_form(
        "pending".into(),
        json!({"type": "object", "properties": {}}),
        None,
    );
    let _request = f.next().await?;
    assert_eq!(f.multiplexer.pending_count(), 1);
    closed.send_replace(true);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), receiver)
            .await?
            .is_err()
    );
    assert_eq!(f.multiplexer.pending_count(), 0);
    Ok(())
}

#[tokio::test]
async fn stale_sender_does_not_deliver_to_a_replacement_with_the_same_textual_id() -> Result<()> {
    let f = fixture("2025-11-25", json!({"elicitation": {}})).await?;
    f.server.remove_session("owner");
    let response = f
        .server
        .handle_request(
            "owner",
            JsonRpcRequest::new(
                json!(1),
                "initialize",
                Some(json!({
                    "protocolVersion": "2025-11-25",
                    "clientInfo": {"name": "replacement", "version": "1"},
                    "capabilities": {"elicitation": {}}
                })),
            ),
        )
        .await
        .context("replacement initialize")?;
    assert!(response.error.is_none(), "{response:?}");
    f.server
        .handle_request(
            "owner",
            JsonRpcRequest::notification("notifications/initialized", None),
        )
        .await;
    assert!(!f.outgoing.supports_form_elicitation());
    let before = f.server.notification_queue_stats().accepted;
    f.outgoing
        .send_event_as_notification(
            &Event {
                id: "old".into(),
                msg: chaos_ipc::protocol::EventMsg::Warning(chaos_ipc::protocol::WarningEvent {
                    message: "private old event".into(),
                }),
            },
            None,
        )
        .await;
    assert_eq!(f.server.notification_queue_stats().accepted, before);
    assert!(
        f.outgoing
            .request_form(
                "old approval".into(),
                json!({"type": "object", "properties": {}}),
                None,
            )
            .await
            .is_err()
    );
    Ok(())
}

async fn fixture(version: &str, capabilities: Value) -> Result<Fixture> {
    fixture_with_request_capacity(version, capabilities, 16).await
}

async fn fixture_with_request_capacity(
    version: &str,
    capabilities: Value,
    capacity: usize,
) -> Result<Fixture> {
    let server = Arc::new(
        Server::builder("test", "1")
            .with_runtime_config(mcp_host::prelude::ServerRuntimeConfig {
                notification_queue_capacity: 4,
                ..Default::default()
            })
            .build(),
    );
    let response = server
        .handle_request(
            "owner",
            JsonRpcRequest::new(
                json!(1),
                "initialize",
                Some(json!({
                    "protocolVersion": version,
                    "clientInfo": {"name": "test", "version": "1"},
                    "capabilities": capabilities
                })),
            ),
        )
        .await
        .context("initialize response")?;
    assert!(response.error.is_none(), "{response:?}");
    server
        .handle_request(
            "owner",
            JsonRpcRequest::notification("notifications/initialized", None),
        )
        .await;
    let (tx, requests) = mpsc::channel(capacity);
    let multiplexer = Arc::new(RequestMultiplexer::new());
    let requester = ClientRequester::new(
        tx,
        multiplexer.clone(),
        Arc::new(server.get_session("owner").context("initialized session")?),
    );
    let urls = Arc::new(UrlElicitations::default());
    let outgoing = OutgoingMessageSender::new(
        Arc::downgrade(&server),
        "owner".into(),
        Some(requester),
        urls.clone(),
    );
    Ok(Fixture {
        server,
        outgoing,
        requests,
        multiplexer,
        urls,
    })
}

impl Fixture {
    async fn next(&mut self) -> Result<JsonRpcClientRequest> {
        tokio::time::timeout(std::time::Duration::from_secs(2), self.requests.recv())
            .await?
            .context("outbound client request")
    }

    fn respond(&self, request: &JsonRpcClientRequest, result: Value) {
        assert!(
            self.multiplexer
                .route_owned_response(JsonRpcResponse::success(json!(request.id), result,))
        );
    }
}

async fn response(receiver: ResponseReceiver) -> Result<Value> {
    receiver
        .await?
        .map_err(|error| anyhow::anyhow!("{error:?}"))
}

#[tokio::test]
async fn full_client_request_queue_fails_without_leaking_or_displacing_requests() -> Result<()> {
    let mut f = fixture_with_request_capacity("2025-11-25", json!({"elicitation": {}}), 1).await?;
    let first = f.outgoing.request_form(
        "first".into(),
        json!({"type": "object", "properties": {}}),
        None,
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while f.requests.len() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let overflow = f.outgoing.request_form(
        "overflow".into(),
        json!({"type": "object", "properties": {}}),
        None,
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), overflow)
            .await??
            .is_err()
    );
    assert_eq!(f.multiplexer.pending_count(), 1);
    let request = f.next().await?;
    assert_eq!(request.params.as_ref().unwrap()["message"], "first");
    f.respond(&request, json!({"action": "decline"}));
    assert_eq!(response(first).await?["action"], "decline");
    assert_eq!(f.multiplexer.pending_count(), 0);
    Ok(())
}

#[tokio::test]
async fn closed_client_request_queue_fails_without_a_pending_request() -> Result<()> {
    let mut f = fixture_with_request_capacity("2025-11-25", json!({"elicitation": {}}), 1).await?;
    f.requests.close();
    let receiver = f.outgoing.request_form(
        "closed".into(),
        json!({"type": "object", "properties": {}}),
        None,
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), receiver)
            .await??
            .is_err()
    );
    assert_eq!(f.multiplexer.pending_count(), 0);
    Ok(())
}

#[tokio::test]
async fn validated_form_requests_preserve_metadata_and_negotiate_versions() -> Result<()> {
    for version in ["2025-06-18", "2025-11-25"] {
        let mut f = fixture(version, json!({"elicitation": {}})).await?;
        let receiver = f.outgoing.request_form(
            "approve?".into(),
            json!({
                "type":"object",
                "properties":{"approved":{"type":"boolean"}},
                "required":["approved"]
            }),
            Some(serde_json::from_value(
                json!({"processId":"p", "codex_call_id":"call"}),
            )?),
        );
        let request = f.next().await?;
        assert_eq!(request.method, "elicitation/create");
        let params = request.params.as_ref().context("form request params")?;
        assert_eq!(params["_meta"]["codex_call_id"], "call");
        assert_eq!(params["_meta"]["processId"], "p");
        if version == "2025-06-18" {
            assert!(params.get("mode").is_none());
        } else {
            assert!(params.get("mode").is_none_or(|mode| mode == "form"));
        }
        f.respond(
            &request,
            json!({"action":"accept", "content":{"approved":true},
                "_meta":{"receipt":"approval", "nested":{"values":[1,2,3]}}}),
        );
        let reply = response(receiver).await?;
        assert_eq!(reply["action"], "accept");
        assert_eq!(
            reply["_meta"],
            json!({
                "receipt":"approval", "nested":{"values":[1,2,3]}
            })
        );

        let receiver = f.outgoing.request_form(
            "approve?".into(),
            json!({
                "type":"object",
                "properties":{"approved":{"type":"boolean"}},
                "required":["approved"]
            }),
            None,
        );
        let request = f.next().await?;
        f.respond(
            &request,
            json!({"action":"accept", "content":{"approved":"yes"}}),
        );
        assert!(
            receiver.await?.is_err(),
            "invalid result must not authorize approval"
        );
    }
    Ok(())
}

#[tokio::test]
async fn missing_capabilities_invalid_schema_and_old_url_mode_never_write() -> Result<()> {
    let mut f = fixture("2025-11-25", json!({})).await?;
    assert!(!f.outgoing.supports_form_elicitation());
    let receiver = f
        .outgoing
        .request_form("ask".into(), json!({"type":"object"}), None);
    assert!(receiver.await?.is_err());
    assert!(f.requests.try_recv().is_err());

    let mut f = fixture("2025-11-25", json!({"elicitation":{}})).await?;
    let receiver = f
        .outgoing
        .request_form("ask".into(), json!({"type":"array"}), None);
    assert!(receiver.await?.is_err());
    assert!(f.requests.try_recv().is_err());

    let mut f = fixture("2025-06-18", json!({"elicitation":{"url":{}}})).await?;
    let receiver = f.outgoing.request_url(
        ProcessId::new(),
        "inner".into(),
        "source".into(),
        "login".into(),
        "https://example.test/auth".into(),
        None,
    );
    assert!(receiver.await?.is_err());
    assert!(f.requests.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn dropping_response_receiver_cancels_pending_request() -> Result<()> {
    let mut f = fixture("2025-11-25", json!({"elicitation":{}})).await?;
    let receiver =
        f.outgoing
            .request_form("ask".into(), json!({"type":"object","properties":{}}), None);
    let _request = f.next().await?;
    assert_eq!(f.multiplexer.pending_count(), 1);
    drop(receiver);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while f.multiplexer.pending_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn url_ids_are_minted_and_completions_are_session_process_and_source_scoped() -> Result<()> {
    let mut f = fixture("2025-11-25", json!({"elicitation":{"url":{}}})).await?;
    let process = ProcessId::new();
    let receiver = f.outgoing.request_url(
        process,
        "inner".into(),
        "source".into(),
        "login".into(),
        "https://example.test/auth?opaque=source".into(),
        None,
    );
    let request = f.next().await?;
    let params = request.params.as_ref().context("URL request params")?;
    let outer = params["elicitationId"]
        .as_str()
        .context("URL id")?
        .to_owned();
    assert_ne!(outer, "source");
    assert_eq!(params["url"], "https://example.test/auth?opaque=source");
    f.respond(
        &request,
        json!({"action":"accept", "_meta":{"receipt":"url-receipt"}}),
    );
    let reply = response(receiver).await?;
    assert_eq!(reply["action"], "accept");
    assert_eq!(reply["_meta"], json!({"receipt":"url-receipt"}));
    assert!(reply.get("content").is_none());
    assert!(
        f.urls
            .complete("other-session", process, "inner".into(), "source".into())
            .is_none()
    );
    assert!(
        f.urls
            .complete("owner", ProcessId::new(), "inner".into(), "source".into())
            .is_none()
    );
    assert!(
        f.urls
            .complete("owner", process, "other-server".into(), "source".into())
            .is_none()
    );
    assert_eq!(
        f.urls
            .complete("owner", process, "inner".into(), "source".into()),
        Some(outer)
    );
    assert!(
        f.urls
            .complete("owner", process, "inner".into(), "source".into())
            .is_none()
    );
    Ok(())
}

#[test]
fn relay_reservations_are_bounded_and_cleaned_on_drop() {
    let urls = Arc::new(UrlElicitations::default());
    let process = ProcessId::new();
    for id in 0..128 {
        let mut entry = urls.reservation("s".into(), process, "inner".into(), id.to_string());
        assert!(entry.register(&id.to_string()));
        entry.retain();
    }
    let mut full = urls.reservation("s".into(), process, "inner".into(), "extra".into());
    assert!(!full.register("extra"));
    assert_eq!(
        urls.complete("s", process, "inner".into(), "0".into())
            .as_deref(),
        Some("0")
    );
    assert!(full.register("extra"));
    drop(full);
    assert!(
        urls.complete("s", process, "inner".into(), "extra".into())
            .is_none()
    );
}

#[tokio::test]
async fn custom_notifications_use_the_bounded_host_queue_without_cross_session_fallback()
-> Result<()> {
    let f = fixture("2025-11-25", json!({})).await?;
    let event = Event {
        id: "event".into(),
        msg: chaos_ipc::protocol::EventMsg::Warning(chaos_ipc::protocol::WarningEvent {
            message: "test".into(),
        }),
    };
    let before = f.server.notification_queue_stats();
    let missing = OutgoingMessageSender::new(
        Arc::downgrade(&f.server),
        "missing-session".into(),
        None,
        f.urls.clone(),
    );
    missing.send_event_as_notification(&event, None).await;
    assert_eq!(
        f.server.notification_queue_stats().accepted,
        before.accepted
    );
    f.outgoing
        .send_event_as_notification(
            &event,
            Some(OutgoingNotificationMeta {
                request_id: Some(RequestId::String("request".into())),
                process_id: None,
            }),
        )
        .await;
    assert_eq!(
        f.server.notification_queue_stats().accepted,
        before.accepted + 1
    );
    for _ in 0..f.server.notification_queue_stats().capacity {
        f.outgoing.send_event_as_notification(&event, None).await;
    }
    let full = f.server.notification_queue_stats();
    assert_eq!(full.queued, full.capacity);
    assert!(full.dropped_full > before.dropped_full);

    let envelope = serde_json::to_value(OutgoingNotificationParams {
        event: &event,
        meta: Some(OutgoingNotificationMeta {
            request_id: Some(RequestId::String("request".into())),
            process_id: None,
        }),
    })?;
    assert_eq!(envelope["_meta"], json!({"requestId":"request"}));
    assert_eq!(envelope["id"], "event");
    Ok(())
}
