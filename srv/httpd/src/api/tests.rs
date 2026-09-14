use super::*;
use chaos_kern::ChaosAuth;
use chaos_kern::config::ConfigBuilder;
use chaos_kern::test_support::auth_manager_from_auth_with_home;
use chaos_kern::test_support::process_table_with_models_provider_and_home;
use rama::futures::stream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Duration;
use tokio::sync::Semaphore;

#[tokio::test]
async fn routes_authenticate_and_bound_ingress_before_starting_processes() {
    let home = tempfile::tempdir().unwrap();
    let config = ConfigBuilder::default()
        .chaos_home(home.path().to_path_buf())
        .fallback_cwd(Some(home.path().to_path_buf()))
        .build()
        .await
        .unwrap();
    let auth = ChaosAuth::from_api_key("test");
    let process_table = process_table_with_models_provider_and_home(
        auth.clone(),
        config.model_provider.clone(),
        home.path().to_path_buf(),
    );
    let state = Arc::new(ServerState {
        config: Arc::new(config),
        process_table: Arc::new(process_table),
        auth_manager: auth_manager_from_auth_with_home(auth, home.path().to_path_buf()),
        bearer_token: Arc::from("test-token"),
        semaphore: Arc::new(Semaphore::new(1)),
        max_concurrent: 1,
        timeout: Duration::from_secs(1),
        body_limit: 8,
        monitor: monitor::MonitorState::new(),
    });
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
    let chunked_body = || {
        let polls = polls.clone();
        Body::from_stream(stream::poll_fn(move |_| {
            polls.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(Some(Ok::<_, Infallible>("1234")))
        }))
    };
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

    tokio::time::pause();
    let stalled = Body::from_stream(stream::pending::<Result<String, Infallible>>());
    assert_eq!(
        handle(state.clone(), trigger(stalled)).await.status(),
        StatusCode::REQUEST_TIMEOUT
    );
    assert_eq!(state.semaphore.available_permits(), 1);
}
