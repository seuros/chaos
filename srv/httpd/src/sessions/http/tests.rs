use super::*;
use crate::test_support::{counting_body, server_state};
use rama::Service;
use rama::http::body::util::BodyExt;
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};

async fn fixture(body_limit: usize) -> (tempfile::TempDir, Arc<ServerState>) {
    server_state(body_limit, true).await
}

fn request(method: &str, path: &str, generation: Option<&str>, body: Body) -> Request {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", "Bearer test-token")
        .header("content-type", "application/json");
    if let Some(generation) = generation {
        request = request.header("x-chaos-daemon-id", generation);
    }
    request.body(body).unwrap()
}

async fn send(state: &Arc<ServerState>, request: Request) -> Response {
    crate::api::http_service(state.clone())
        .serve(request)
        .await
        .unwrap()
}

async fn json(response: Response) -> serde_json::Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn bearer_and_generation_are_checked_before_reading_bodies() {
    let (_home, state) = fixture(16).await;
    let polls = Arc::new(AtomicUsize::new(0));
    let body = || counting_body(&polls);
    let mut unauthenticated = request("POST", "/api/sessions", None, body());
    unauthenticated.headers_mut().remove("authorization");
    assert_eq!(
        send(&state, unauthenticated).await.status(),
        StatusCode::UNAUTHORIZED
    );
    for (generation, expected) in [
        (None, StatusCode::PRECONDITION_REQUIRED),
        (Some("previous-daemon"), StatusCode::CONFLICT),
    ] {
        assert_eq!(
            send(&state, request("POST", "/api/sessions", generation, body()))
                .await
                .status(),
            expected
        );
    }
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    let manager = state.sessions.as_ref().unwrap();
    let mut oversized = request("POST", "/api/sessions", Some(&manager.daemon_id), body());
    oversized
        .headers_mut()
        .insert("content-length", "17".parse().unwrap());
    assert_eq!(
        send(&state, oversized).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    let chunked = request("POST", "/api/sessions", Some(&manager.daemon_id), body());
    assert_eq!(
        send(&state, chunked).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(polls.load(Ordering::SeqCst), 5);
    assert!(manager.snapshots().is_empty());
    assert!(state.process_table.list_process_ids().await.is_empty());
}

#[tokio::test]
async fn body_deadlines_and_ingress_capacity_do_not_start_sessions() {
    let (_home, state) = fixture(16).await;
    // Config construction opens a real SQLite pool. Pause only after that
    // boundary, so virtual timers cannot outrun the connection worker.
    tokio::time::pause();
    let manager = state.sessions.as_ref().unwrap();
    let permits = manager.ingress.acquire_many(16).await.unwrap();
    let pending =
        || Body::from_stream(rama::futures::stream::pending::<Result<String, Infallible>>());
    let value = send(
        &state,
        request("POST", "/api/sessions", Some(&manager.daemon_id), pending()),
    )
    .await;
    assert_eq!(value.status(), StatusCode::TOO_MANY_REQUESTS);
    drop(permits);
    let value = send(
        &state,
        request("POST", "/api/sessions", Some(&manager.daemon_id), pending()),
    )
    .await;
    assert_eq!(value.status(), StatusCode::REQUEST_TIMEOUT);
    let body = json(value).await;
    // Shared ingress must not erase the session endpoint's wire contract.
    assert_eq!(body["status"], "error");
    assert_eq!(body["error"], "request body deadline exceeded");
    assert_eq!(body["daemon_id"], manager.daemon_id);
    assert_eq!(manager.ingress.available_permits(), 16);
    assert!(manager.snapshots().is_empty());
}

#[tokio::test]
async fn turn_http_contract_is_retry_safe_and_controls_bypass_busy_execution_capacity() {
    let (_home, state) = fixture(1024).await;
    let manager = state.sessions.as_ref().unwrap();
    let (session, mut commands) = manager.reserve(None).unwrap();
    session.data.lock().unwrap().status = SessionStatus::Idle;
    let turn_id = Uuid::new_v4().to_string();
    let path = format!("/api/sessions/{}/turns/{turn_id}", session.id);
    let submit = |text| request("POST", &path, Some(&manager.daemon_id), Body::from(text));
    for _ in 0..2 {
        let value = send(&state, submit(r#"{"request":"hello"}"#)).await;
        assert_eq!(value.status(), StatusCode::ACCEPTED);
        assert_eq!(json(value).await["turn"]["status"], "accepted");
    }
    assert_eq!(state.semaphore.available_permits(), 0);
    assert!(
        matches!(commands.recv().await, Some(super::super::Command::Turn(id)) if id == turn_id)
    );
    assert!(commands.try_recv().is_err());
    assert_eq!(
        send(&state, submit(r#"{"request":"changed"}"#))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let next = format!("/api/sessions/{}/turns/{}", session.id, Uuid::new_v4());
    assert_eq!(
        send(
            &state,
            request(
                "POST",
                &next,
                Some(&manager.daemon_id),
                Body::from(r#"{"request":"next"}"#)
            )
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    let interrupt = format!("{path}/interrupt");
    assert_eq!(
        send(
            &state,
            request("POST", &interrupt, Some(&manager.daemon_id), Body::empty())
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(state.semaphore.available_permits(), 0);
    assert!(
        matches!(commands.recv().await, Some(super::super::Command::Interrupt(id)) if id == turn_id)
    );
    let value = send(&state, request("GET", &path, None, Body::empty())).await;
    assert_eq!(value.headers()["cache-control"], "no-store");
    assert_eq!(json(value).await["turn"]["status"], "interrupting");
    let value = send(
        &state,
        request("PUT", &path, Some(&manager.daemon_id), Body::empty()),
    )
    .await;
    assert_eq!(value.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(value.headers()["allow"], "GET, POST");
}

#[tokio::test]
async fn request_schemas_reject_unsafe_overrides_and_invalid_identifiers() {
    let (_home, state) = fixture(1024).await;
    let manager = state.sessions.as_ref().unwrap();
    for body in [
        r#"{"provider":"other"}"#,
        r#"{"env":{"SECRET":"value"}}"#,
        r#"{"sandbox":"danger-full-access"}"#,
        r#"{"resume_process_id":"not-a-uuid"}"#,
    ] {
        assert_eq!(
            send(
                &state,
                request(
                    "POST",
                    "/api/sessions",
                    Some(&manager.daemon_id),
                    Body::from(body)
                )
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        send(
            &state,
            request("GET", "/api/sessions/not-a-uuid", None, Body::empty())
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    assert!(manager.snapshots().is_empty());
}

#[tokio::test]
async fn event_sse_replays_a_cursor_and_closes_after_session_shutdown() {
    let (_home, state) = fixture(1024).await;
    let manager = state.sessions.as_ref().unwrap();
    let (session, _commands) = manager.reserve(None).unwrap();
    for text in ["first", "second"] {
        session.observe(&chaos_ipc::protocol::Event {
            id: String::new(),
            msg: chaos_ipc::protocol::EventMsg::BackgroundEvent(
                chaos_ipc::protocol::BackgroundEventEvent {
                    message: text.into(),
                },
            ),
        });
    }
    session.data.lock().unwrap().status = SessionStatus::Closed;
    let mut value = request(
        "GET",
        &format!("/api/sessions/{}/events", session.id),
        None,
        Body::empty(),
    );
    value
        .headers_mut()
        .insert("accept", "text/event-stream".parse().unwrap());
    value
        .headers_mut()
        .insert("last-event-id", "1".parse().unwrap());
    let value = send(&state, value).await;
    assert_eq!(value.status(), StatusCode::OK);
    assert_eq!(value.headers()["cache-control"], "no-store");
    let body = value.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("id: 2\nevent: kernel"));
    assert!(!body.contains("first"));
    assert!(body.contains("second"));
    assert!(body.contains("event: session"));
    assert!(body.contains("\"status\":\"closed\""));
    let invalid = request(
        "GET",
        &format!("/api/sessions/{}/events?after=3", session.id),
        None,
        Body::empty(),
    );
    assert_eq!(
        send(&state, invalid).await.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn daemon_shutdown_closes_unknown_streams_without_claiming_containment() {
    let (_home, state) = fixture(1024).await;
    let manager = state.sessions.as_ref().unwrap();
    let (session, _commands) = manager.reserve(None).unwrap();
    session.data.lock().unwrap().status = SessionStatus::Unknown;
    manager.shutdown.send_replace(true);
    let mut value = request(
        "GET",
        &format!("/api/sessions/{}/events", session.id),
        None,
        Body::empty(),
    );
    value
        .headers_mut()
        .insert("accept", "text/event-stream".parse().unwrap());
    let value = send(&state, value).await;
    let body = value.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("\"shutting_down\":true"));
    assert!(body.contains("\"status\":\"unknown\""));
    assert!(!body.contains("\"status\":\"closed\""));
    let value = request(
        "POST",
        "/api/sessions",
        Some(&manager.daemon_id),
        Body::from("{}"),
    );
    assert_eq!(
        send(&state, value).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(manager.reserve(None).is_err());
}

#[tokio::test]
async fn notice_ids_share_one_url_safe_alphabet_on_post_and_delete() {
    // Mira's review: POST accepted `n+1`, which no DELETE ?id= could name.
    let (_home, state) = fixture(1024).await;
    let manager = state.sessions.as_ref().unwrap();
    let (session, mut commands) = manager.reserve(None).unwrap();
    session.data.lock().unwrap().status = SessionStatus::Idle;
    let turn_id = Uuid::new_v4().to_string();
    let path = format!("/api/sessions/{}/turns/{turn_id}", session.id);
    let value = send(
        &state,
        request(
            "POST",
            &path,
            Some(&manager.daemon_id),
            Body::from(r#"{"request":"hello"}"#),
        ),
    )
    .await;
    assert_eq!(value.status(), StatusCode::ACCEPTED);
    assert!(matches!(
        commands.recv().await,
        Some(super::super::Command::Turn(_))
    ));
    let notice = format!("{path}/notice");
    for id in ["n+1", "n%2B1", "n 1", "", "n/1"] {
        let body = serde_json::json!({"id": id, "text": "post a line"}).to_string();
        assert_eq!(
            send(
                &state,
                request("POST", &notice, Some(&manager.daemon_id), Body::from(body))
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST,
            "POST id {id:?}"
        );
    }
    for query in ["id=n+1", "id=n%2B1", "id=", "other=1", "id=a&id=b"] {
        assert_eq!(
            send(
                &state,
                request(
                    "DELETE",
                    &format!("{notice}?{query}"),
                    Some(&manager.daemon_id),
                    Body::empty()
                )
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST,
            "DELETE ?{query}"
        );
    }
    // Every id POST accepts can be named by DELETE ?id=.
    let responder = tokio::spawn(async move {
        match commands.recv().await {
            Some(super::super::Command::Notice { change, reply, .. }) => {
                let named = matches!(
                    &change,
                    super::super::NoticeChange::Withdraw(Some(id)) if id == "n-1.a_b~c"
                );
                let _ = reply.send(Ok(false));
                named
            }
            _ => false,
        }
    });
    let value = send(
        &state,
        request(
            "DELETE",
            &format!("{notice}?id=n-1.a_b~c"),
            Some(&manager.daemon_id),
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(value.status(), StatusCode::OK);
    assert_eq!(json(value).await["status"], "notice_not_pending");
    assert!(responder.await.unwrap(), "DELETE named the URL-safe id");
}
