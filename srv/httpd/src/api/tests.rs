use super::*;
use crate::test_support::{counting_body, server_state};
use rama::futures::stream;
use rama::http::body::util::BodyExt;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(not(feature = "mcpd"))]
#[tokio::test]
async fn mcp_routes_are_absent_without_the_daemon_feature() {
    let (_home, state) = server_state(8, false).await;
    let polls = Arc::new(AtomicUsize::new(0));
    for method in ["GET", "POST", "DELETE", "OPTIONS", "PUT"] {
        for path in ["/mcp", "/mcp/", "/mcp/tools", "/mcpx"] {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", "Bearer test-token")
                .body(counting_body(&polls))
                .unwrap();
            let response = handle(state.clone(), request).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
            assert!(!response.headers().contains_key("mcp-session-id"));
        }
    }
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    let response = handle(
        state,
        Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn routes_authenticate_and_bound_ingress_before_starting_processes() {
    let (_home, state) = server_state(8, false).await;
    for (method, path, bearer, expected) in [
        ("GET", "/api/health", None, StatusCode::OK),
        ("GET", "/monitor", None, StatusCode::OK),
        ("GET", "/monitor/events", None, StatusCode::UNAUTHORIZED),
        (
            "GET",
            "/monitor/events?token=test-token",
            None,
            StatusCode::UNAUTHORIZED,
        ),
        (
            "GET",
            "/monitor/events",
            Some("Bearer wrong"),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "GET",
            "/monitor/events",
            Some("Bearer test-token"),
            StatusCode::OK,
        ),
        ("POST", "/api/trigger", None, StatusCode::UNAUTHORIZED),
        (
            "POST",
            "/api/trigger",
            Some("Bearer wrong"),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "GET",
            "/api/trigger",
            Some("Bearer test-token"),
            StatusCode::METHOD_NOT_ALLOWED,
        ),
    ] {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(bearer) = bearer {
            request = request.header("authorization", bearer);
        }
        let response = handle(state.clone(), request.body(Body::empty()).unwrap()).await;
        assert_eq!(response.status(), expected, "{method} {path}");
        if expected == StatusCode::UNAUTHORIZED {
            assert_eq!(response.headers()["www-authenticate"], "Bearer");
        } else if path == "/monitor/events" && expected == StatusCode::OK {
            assert_eq!(response.headers()["cache-control"], "no-store");
            let frame = response.into_body().frame().await.unwrap().unwrap();
            assert!(String::from_utf8_lossy(frame.data_ref().unwrap()).contains("monitor-summary"));
        }
    }

    let trigger = |body| {
        Request::builder()
            .method("POST")
            .uri("/api/trigger")
            .header("authorization", "Bearer test-token")
            .header("content-type", "application/json")
            .body(body)
            .unwrap()
    };
    let polls = Arc::new(AtomicUsize::new(0));
    let chunked_body = || counting_body(&polls);
    for content_type in ["text/plain", "application/json-patch+json"] {
        let mut invalid = trigger(chunked_body());
        invalid
            .headers_mut()
            .insert("content-type", content_type.parse().unwrap());
        assert_eq!(
            handle(state.clone(), invalid).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    // Both header preflight and capacity rejection must leave the stream
    // entirely unread. In particular, 429 cannot depend on JSON parsing.
    let mut oversized = trigger(chunked_body());
    oversized
        .headers_mut()
        .insert("content-length", "9".parse().unwrap());
    assert_eq!(
        handle(state.clone(), oversized).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let permit = state.semaphore.acquire().await.unwrap();
    assert_eq!(
        handle(state.clone(), trigger(chunked_body()))
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    drop(permit);
    // An endless chunked stream stops at the first overflowing frame,
    // rather than merely returning 413 after collecting the entire body.
    assert_eq!(
        handle(state.clone(), trigger(chunked_body()))
            .await
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(polls.load(Ordering::SeqCst), 3);
    assert_eq!(state.semaphore.available_permits(), 1);
    assert_eq!(
        handle(state.clone(), trigger(Body::from("{}")))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let mut parameters = trigger(Body::from("{}"));
    parameters.headers_mut().insert(
        "content-type",
        "Application/JSON; charset=utf-8".parse().unwrap(),
    );
    let response = handle(state.clone(), parameters).await;
    let body: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["error"], "missing required field: request");

    tokio::time::pause();
    let stalled = Body::from_stream(stream::pending::<Result<String, Infallible>>());
    let response = handle(state.clone(), trigger(stalled)).await;
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    let body: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["status"], "timeout");
    assert_eq!(body["error"], "request body deadline exceeded");
    assert_eq!(state.semaphore.available_permits(), 1);
}
