use super::*;
use crate::webhooks::WebhookConfig;
use chaos_journald::{
    ExecutionDelivery, ExecutionDeliveryRequest as DeliveryRequest, ExecutionDeliveryStatus,
    ExecutionEvent,
};
use hmac::{Hmac, KeyInit, Mac};
use rama::{
    http::{Body, HeaderMap, Request, Response, body::util::BodyExt, server::HttpServer},
    rt::Executor,
    service::service_fn,
    tcp::server::TcpListener,
};
use sha2::Sha256;
use std::convert::Infallible;

const KEY: &str = "test-webhook-key-not-a-real-secret-12345";

async fn deliveries(fixture: &Fixture) -> Vec<ExecutionDeliveryStatus> {
    let ExecutionResponse::Deliveries(records) = fixture
        .store
        .call(ExecutionRequest::Delivery {
            request: DeliveryRequest::List {
                scope: fixture.store.scope.clone(),
                after: None,
                limit: 256,
            },
        })
        .await
        .unwrap()
    else {
        panic!("expected deliveries")
    };
    records
}

pub(super) async fn terminal(fixture: &Fixture, id: Uuid) {
    let owner = fixture.session.ledger.get().unwrap();
    fixture.admit(&id.to_string()).await.unwrap();
    owner.dispatch(&id.to_string()).await.unwrap();
    owner
        .finish(TurnSnapshot {
            turn_id: id.to_string(),
            status: TurnStatus::Completed,
            result: Some("private result".into()),
            result_truncated: false,
            error: None,
            usage: None,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn outbox_insert_failure_rolls_back_terminal_outcome_and_keeps_capacity() {
    let mut fixture = Fixture::new().await;
    let mut supervisor = fixture.start();
    let id = Uuid::new_v4().to_string();
    fixture.admit(&id).await.unwrap();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Execute(_, _)
    ));
    fixture.db.fail_outbox(false).await;
    supervisor.events.send(complete(&id)).unwrap();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Shutdown
    ));
    status(&fixture.session, SessionStatus::Unknown).await;
    assert!(!fixture.persisted_turn(&id).await.state.is_terminal());
    assert!(deliveries(&fixture).await.is_empty());
    assert_eq!(fixture.capacity.available_permits(), 0);
    supervisor.task.abort();
    let _ = supervisor.task.await;
    fixture.db.close().await;
}

#[tokio::test]
async fn recovery_and_no_turn_notification_commit_together_without_duplicate_events() {
    for turn in [false, true] {
        let (fixture, body) = Fixture::quarantined(turn).await;
        let mut request: crate::sessions::durable::recovery::ResolveRequest =
            serde_json::from_value(body).unwrap();
        fixture.db.fail_outbox(false).await;
        assert!(fixture.session.resolve(request.clone()).await.is_err());
        assert_eq!(fixture.capacity.available_permits(), 0);
        assert!(deliveries(&fixture).await.is_empty());
        assert!(matches!(
            fixture
                .store
                .call(ExecutionRequest::GetResolution {
                    session_id: Uuid::parse_str(&fixture.session.id).unwrap()
                })
                .await
                .unwrap(),
            ExecutionResponse::Resolution(None)
        ));
        fixture.db.clear_outbox_fault().await;
        fixture.db.expire(&fixture.session.id).await;
        let (_manager, recovered, capacity) = fixture.restored().await;
        assert!(
            recovered.resolve(request.clone()).await.is_err(),
            "takeover invalidates old assertions"
        );
        // The fake executor remains independently contained; reconfirm this incarnation.
        request.expected_recovery_epoch = fixture.epoch().await;
        recovered.resolve(request.clone()).await.unwrap();
        let original = deliveries(&fixture).await;
        assert_eq!(original.len(), 1);
        assert_eq!(
            original[0].kind,
            if turn {
                "turn.terminal"
            } else {
                "session.resolved"
            }
        );
        recovered.resolve(request.clone()).await.unwrap();
        fixture.session.resolve(request).await.unwrap();
        assert_eq!(deliveries(&fixture).await, original);
        assert_eq!(fixture.capacity.available_permits(), 1);
        assert_eq!(capacity.available_permits(), 1);
        fixture.db.close().await;
    }
}

#[tokio::test]
async fn delivery_claims_are_fenced_and_closed_history_cannot_be_rerouted() {
    let fixture = Fixture::new().await;
    let turn_id = Uuid::new_v4();
    terminal(&fixture, turn_id).await;
    let scope = fixture.store.scope.clone();
    let digest = "d".repeat(64);
    let configure = DeliveryRequest::Configure {
        scope: scope.clone(),
        destination_digest: digest.clone(),
    };
    fixture
        .store
        .call(ExecutionRequest::Delivery {
            request: configure.clone(),
        })
        .await
        .unwrap();
    let claim = |owner: &str| ExecutionRequest::Delivery {
        request: DeliveryRequest::Claim {
            scope: scope.clone(),
            destination_digest: digest.clone(),
            owner_id: owner.into(),
            ttl_ms: 60_000,
        },
    };
    let (a, b) = tokio::join!(
        fixture.store.call(claim("one")),
        fixture.store.call(claim("two"))
    );
    let held: Vec<ExecutionDelivery> = [a.unwrap(), b.unwrap()]
        .into_iter()
        .filter_map(|r| {
            if let ExecutionResponse::Delivery(delivery) = r {
                delivery
            } else {
                panic!("expected claim")
            }
        })
        .collect();
    assert_eq!(held.len(), 1);
    let old = held.into_iter().next().unwrap();
    // The turn's idempotent finish and lease release cannot remove delivery history.
    fixture
        .session
        .ledger
        .get()
        .unwrap()
        .finish(
            serde_json::from_value(
                fixture
                    .persisted_turn(&turn_id.to_string())
                    .await
                    .outcome
                    .unwrap(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deliveries(&fixture).await.len(), 1);
    fixture
        .session
        .ledger
        .get()
        .unwrap()
        .retire(false)
        .await
        .unwrap();
    assert!(
        fixture
            .store
            .call(ExecutionRequest::Delivery {
                request: DeliveryRequest::Configure {
                    scope: scope.clone(),
                    destination_digest: "e".repeat(64),
                }
            })
            .await
            .is_err()
    );
    fixture.db.make_delivery_due(old.status.event_id).await;
    let ExecutionResponse::Delivery(Some(new)) =
        fixture.store.call(claim("new-owner")).await.unwrap()
    else {
        panic!("expected reclaim")
    };
    assert_eq!(new.body_json, old.body_json);
    assert_ne!(new.lease.token, old.lease.token);
    assert!(
        fixture
            .store
            .call(ExecutionRequest::Delivery {
                request: DeliveryRequest::Acknowledge {
                    scope: scope.clone(),
                    destination_digest: digest.clone(),
                    lease: old.lease,
                }
            })
            .await
            .is_err()
    );
    let ack = ExecutionRequest::Delivery {
        request: DeliveryRequest::Acknowledge {
            scope: scope.clone(),
            destination_digest: digest.clone(),
            lease: new.lease.clone(),
        },
    };
    fixture.store.call(ack.clone()).await.unwrap();
    let saved = deliveries(&fixture).await;
    fixture.store.call(ack).await.unwrap();
    assert_eq!(deliveries(&fixture).await, saved);
    assert!(matches!(
        fixture.store.call(claim("last")).await.unwrap(),
        ExecutionResponse::Delivery(None)
    ));
    let event: ExecutionEvent = serde_json::from_str(&new.body_json).unwrap();
    assert_eq!(event.turn.unwrap().state, ExecutionTurnState::Completed);
    fixture.db.close().await;
}

#[tokio::test]
async fn signed_webhook_retry_survives_lost_ack_and_never_follows_redirect_or_replays_execution() {
    let mut fixture = Fixture::new().await;
    let mut supervisor = fixture.start();
    let turn_id = Uuid::new_v4().to_string();
    fixture.admit(&turn_id).await.unwrap();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Execute(_, _)
    ));
    supervisor.events.send(complete(&turn_id)).unwrap();
    status(&fixture.session, SessionStatus::Idle).await;
    let event_id = deliveries(&fixture).await[0].event_id;

    let redirect = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let redirect_url = format!("http://{}/stolen", redirect.local_addr().unwrap());
    let listener = TcpListener::build(Executor::default())
        .bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .await
        .unwrap();
    let url = format!("http://{}/terminal", listener.local_addr().unwrap());
    let (received, mut requests) = mpsc::unbounded_channel::<(HeaderMap, Vec<u8>)>();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let service = service_fn(move |request: Request| {
        let received = received.clone();
        let redirect_url = redirect_url.clone();
        let first = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
        async move {
            let (parts, body) = request.into_parts();
            let bytes = body.collect().await.unwrap().to_bytes();
            received.send((parts.headers, bytes.to_vec())).unwrap();
            Ok::<_, Infallible>(
                Response::builder()
                    .status(if first { 302 } else { 204 })
                    .header("location", redirect_url)
                    .body(Body::empty())
                    .unwrap(),
            )
        }
    });
    let receiver =
        tokio::spawn(listener.serve(HttpServer::new_http1(Executor::default()).service(service)));
    let config = || WebhookConfig::new(&url, KEY.into()).unwrap();
    config().configure(&fixture.manager).await.unwrap();
    assert!(
        WebhookConfig::new(&url, format!("{KEY}-changed"))
            .unwrap()
            .configure(&fixture.manager)
            .await
            .is_err()
    );
    fixture.db.fail_outbox(true).await;
    let worker = config().spawn(fixture.manager.clone());
    let mut bodies = Vec::new();
    for _ in 0..2 {
        let (headers, body) = tokio::time::timeout(Duration::from_secs(10), requests.recv())
            .await
            .unwrap()
            .unwrap();
        verify_signature(&headers, &body);
        assert_eq!(headers["x-chaos-event-id"], event_id.to_string());
        assert!(!headers.contains_key("authorization") && !headers.contains_key("cookie"));
        bodies.push(body);
    }
    assert_eq!(
        bodies[0], bodies[1],
        "redirect/non-success retries must preserve exact immutable bytes"
    );
    // Let the failed delivery acknowledgement finish before simulating a crash.
    tokio::time::sleep(Duration::from_millis(100)).await;
    worker.abort();
    let _ = worker.await;
    assert!(deliveries(&fixture).await[0].delivered_at_ms.is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), redirect.accept())
            .await
            .is_err(),
        "private event must not follow Location"
    );
    fixture.db.clear_outbox_fault().await;
    fixture.db.make_delivery_due(event_id).await;
    let restarted = config().spawn(fixture.manager.clone());
    let (headers, body) = tokio::time::timeout(Duration::from_secs(5), requests.recv())
        .await
        .unwrap()
        .unwrap();
    verify_signature(&headers, &body);
    assert_eq!(
        body, bodies[0],
        "receiver may observe the same event again after a lost ack"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while deliveries(&fixture).await[0].delivered_at_ms.is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        supervisor.operations.try_recv().is_err(),
        "delivery retries cannot submit another turn"
    );
    assert_eq!(fixture.capacity.available_permits(), 1);
    fixture.manager.close_all();
    restarted.await.unwrap();
    assert!(matches!(
        operation(&mut supervisor).await,
        Operation::Shutdown
    ));
    supervisor.task.await.unwrap();
    receiver.abort();
    let _ = receiver.await;
    fixture.db.close().await;
}

fn verify_signature(headers: &HeaderMap, body: &[u8]) {
    let hex = headers["x-chaos-signature"]
        .to_str()
        .unwrap()
        .strip_prefix("v1=")
        .unwrap();
    let signature: Vec<_> = hex
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    let mut verifier = Hmac::<Sha256>::new_from_slice(KEY.as_bytes()).unwrap();
    verifier.update(headers["x-chaos-timestamp"].as_bytes());
    verifier.update(b".");
    verifier.update(headers["x-chaos-event-id"].as_bytes());
    verifier.update(b".");
    verifier.update(body);
    verifier.verify_slice(&signature).unwrap();
}
