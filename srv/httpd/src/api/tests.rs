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
        ("GET", "/monitor/app.css", None, StatusCode::OK),
        (
            "POST",
            "/monitor/app.css",
            None,
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        // Read-only and redacted: open unless a viewer token is set.
        ("GET", "/monitor/events", None, StatusCode::OK),
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
            let frame = String::from_utf8_lossy(frame.data_ref().unwrap()).into_owned();
            assert!(
                frame.starts_with("event: datastar-patch-elements\n"),
                "{frame}"
            );
            assert!(
                frame.contains("data: elements <main id=\"dashboard\""),
                "{frame}"
            );
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

fn viewer(path: &str, host: &str, bearer: Option<&str>) -> Request {
    let mut request = Request::builder().uri(path).header("host", host);
    if let Some(bearer) = bearer {
        request = request.header("authorization", format!("Bearer {bearer}"));
    }
    request.body(Body::empty()).unwrap()
}

async fn page(state: &Arc<ServerState>, host: &str) -> String {
    let response = handle(state.clone(), viewer("/monitor", host, None)).await;
    let body = response.into_body().collect().await.unwrap();
    String::from_utf8(body.to_bytes().to_vec()).unwrap()
}

#[tokio::test]
async fn the_monitor_is_open_unless_a_viewer_token_is_set() {
    use crate::monitor::Access;
    let (_a, open) = crate::test_support::server_state(8, false).await;
    let (_b, loopback) = crate::test_support::server_state_with(
        8,
        false,
        Access {
            token: None,
            loopback_bind: true,
        },
    )
    .await;
    let (_c, shared) = crate::test_support::server_state_with(
        8,
        false,
        Access {
            token: Some(Arc::from("viewer-token")),
            loopback_bind: false,
        },
    )
    .await;
    for (state, host, bearer, expected) in [
        (&open, "10.0.0.7:4000", None, StatusCode::OK),
        (&loopback, "127.0.0.1:4000", None, StatusCode::OK),
        (&loopback, "[::1]:4000", None, StatusCode::OK),
        (&loopback, "localhost:4000", None, StatusCode::OK),
        // A page that rebinds its own name to 127.0.0.1 keeps its Host.
        (&loopback, "evil.example:4000", None, StatusCode::FORBIDDEN),
        (
            &loopback,
            "localhost.evil.example",
            None,
            StatusCode::FORBIDDEN,
        ),
        (&shared, "10.0.0.7:4000", None, StatusCode::UNAUTHORIZED),
        (
            &shared,
            "10.0.0.7:4000",
            Some("wrong"),
            StatusCode::UNAUTHORIZED,
        ),
        (
            &shared,
            "10.0.0.7:4000",
            Some("viewer-token"),
            StatusCode::OK,
        ),
        (&shared, "10.0.0.7:4000", Some("test-token"), StatusCode::OK),
    ] {
        let response = handle(state.clone(), viewer("/monitor/events", host, bearer)).await;
        assert_eq!(response.status(), expected, "{host} {bearer:?}");
    }

    let mut post = viewer("/api/trigger", "10.0.0.7:4000", Some("viewer-token"));
    *post.method_mut() = rama::http::Method::POST;
    assert_eq!(
        handle(shared.clone(), post).await.status(),
        StatusCode::UNAUTHORIZED,
        "the viewer token never runs agents"
    );

    assert!(page(&open, "10.0.0.7:4000").await.contains("data-init="));
    assert!(
        page(&loopback, "localhost:4000")
            .await
            .contains("data-init=")
    );
    assert!(!page(&loopback, "evil.example").await.contains("data-init="));
    assert!(!page(&shared, "10.0.0.7:4000").await.contains("data-init="));
}
