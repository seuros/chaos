use std::ffi::OsStr;
use std::sync::atomic::{AtomicUsize, Ordering};

use chaos_argv::Arg0DispatchPaths;
use mcp_host::transport::http::HttpBearerAuthSet;
use rama::http::body::util::BodyExt;
use rama::http::{Body, StatusCode};
use serde_json::{Value, json};

use super::*;
use crate::test_support::{counting_body, server_state};

mod workflow_http;

#[derive(Debug, usage::Cli)]
#[usage(bin = "httpd-test")]
struct TestCli {
    #[usage(flatten)]
    serve: ServeCli,
}

fn cli(args: &[&str]) -> ServeCli {
    let argv = args.iter().map(OsStr::new).collect::<Vec<_>>();
    TestCli::parse_from(&argv).unwrap().serve
}

fn request(method: &str, token: Option<&str>, body: Body) -> Request {
    let mut request = Request::builder().method(method).uri("/mcp");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    request.body(body).unwrap()
}

fn post(session: Option<&str>, value: Value) -> Request {
    let mut request = request(
        "POST",
        Some("test-token"),
        Body::from(serde_json::to_string(&value).unwrap()),
    );
    let headers = request.headers_mut();
    headers.insert("content-type", "application/json".parse().unwrap());
    headers.insert(
        "accept",
        "application/json, text/event-stream".parse().unwrap(),
    );
    if let Some(session) = session {
        headers.insert("mcp-session-id", session.parse().unwrap());
        headers.insert("mcp-protocol-version", "2025-11-25".parse().unwrap());
    }
    request
}

fn initialize() -> Value {
    json!({
        "jsonrpc":"2.0", "id":1, "method":"initialize",
        "params":{
            "protocolVersion":"2025-11-25",
            "capabilities":{},
            "clientInfo":{"name":"httpd-test","version":"1"}
        }
    })
}

async fn answer(response: Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = tokio::time::timeout(Duration::from_secs(5), response.into_body().collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes();
    let text = std::str::from_utf8(&bytes).unwrap();
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
        .find(|message| message.get("id").is_some())
        .unwrap_or_else(|| serde_json::from_str(text).unwrap())
}

async fn mount() -> (tempfile::TempDir, Arc<crate::ServerState>) {
    let (home, mut state) = server_state(4096, false).await;
    let kernel = chaos_mcpd::build_server(
        state.config.clone(),
        Arg0DispatchPaths {
            alcatraz_exe: std::path::PathBuf::from("/unused-test-alcatraz"),
        },
    )
    .await
    .unwrap();
    Arc::get_mut(&mut state).unwrap().mcp = Some(
        McpEndpoint::new(
            kernel,
            &cli(&["--mcp", "--body-limit", "4096"]),
            "test-token",
        )
        .unwrap(),
    );
    (home, state)
}

#[test]
fn flags_default_off_repeat_origins_and_refuse_recovery_only() {
    let defaults = cli(&[]);
    assert!(!defaults.mcp);
    assert!(defaults.mcp_allowed_origin.is_empty());
    assert!(validate_cli(&defaults).is_ok());
    let enabled = cli(&[
        "--mcp",
        "--mcp-allowed-origin",
        "https://one.example",
        "--mcp-allowed-origin",
        "https://two.example",
    ]);
    assert!(enabled.mcp);
    assert_eq!(
        enabled.mcp_allowed_origin,
        ["https://one.example", "https://two.example"]
    );
    assert!(validate_cli(&enabled).is_ok());
    assert!(validate_cli(&cli(&["--mcp", "--session-recovery-only"])).is_err());
    assert!(validate_cli(&cli(&["--mcp-allowed-origin", "https://one.example"])).is_err());
    for invalid in ["*", "null", "file:///tmp", "https://one.example/path"] {
        assert!(validate_cli(&cli(&["--mcp", "--mcp-allowed-origin", invalid])).is_err());
    }
    let config = transport_config(&enabled, "test-token").unwrap();
    assert_eq!(config.max_request_body_size, enabled.body_limit);
    assert_eq!(config.max_sessions, enabled.max_sessions);
    assert_eq!(config.max_sse_connections, enabled.max_concurrent);
    assert_eq!(
        config.session_timeout,
        Duration::from_secs(enabled.session_ttl)
    );
    assert_eq!(config.request_timeout, Duration::from_secs(enabled.timeout));
    assert!(transport_config(&enabled, "invalid bearer token").is_err());
}

#[tokio::test]
async fn disabled_endpoint_and_nearby_paths_preserve_existing_routes() {
    let (_home, state) = server_state(4096, false).await;
    let service = crate::api::http_service(state);
    for path in ["/mcp", "/mcp/", "/mcp/tools", "/mcpx"] {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            service.serve(request).await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        service
            .serve(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn sdk_authenticates_every_method_before_reading_and_rejects_duplicate_headers() {
    let (_home, state) = mount().await;
    let service = crate::api::http_service(state.clone());
    let polls = Arc::new(AtomicUsize::new(0));
    for method in ["POST", "GET", "DELETE", "PATCH", "HEAD"] {
        for token in [None, Some("wrong"), Some("recovery-token")] {
            let response = service
                .serve(request(method, token, counting_body(&polls)))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{method}");
            assert_eq!(response.headers()["www-authenticate"], "Bearer");
        }
    }
    for bearer in ["Basic test-token", "Bearer", "Bearer test-token other"] {
        let mut invalid = request("POST", None, counting_body(&polls));
        invalid
            .headers_mut()
            .insert("authorization", bearer.parse().unwrap());
        assert_eq!(
            service.serve(invalid).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    for header in [
        "authorization",
        "origin",
        "content-type",
        "mcp-session-id",
        "mcp-protocol-version",
        "last-event-id",
    ] {
        let mut duplicate = request("POST", Some("test-token"), counting_body(&polls));
        let value = if header == "authorization" {
            "Bearer test-token"
        } else {
            "same"
        };
        duplicate
            .headers_mut()
            .append(header, value.parse().unwrap());
        duplicate
            .headers_mut()
            .append(header, value.parse().unwrap());
        assert_eq!(
            service.serve(duplicate).await.unwrap().status(),
            StatusCode::BAD_REQUEST,
            "{header}"
        );
    }
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert_eq!(
        service
            .serve(request("PATCH", Some("test-token"), Body::empty()))
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    state.shutdown_sessions().await;
}

#[tokio::test]
async fn shared_post_ingress_rejects_unread_bodies_but_keeps_controls_available() {
    let (_home, state) = mount().await;
    let service = crate::api::http_service(state.clone());
    let initialized = service.serve(post(None, initialize())).await.unwrap();
    let session = initialized.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    // The response still owns its unread SSE body, but not the ingress permit.
    assert_eq!(state.semaphore.available_permits(), 1);
    answer(initialized).await;

    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    let body = Body::from_stream(rama::futures::stream::once(async move {
        let _ = started_tx.send(());
        resume_rx.await.unwrap();
        Ok::<_, std::convert::Infallible>(serde_json::to_string(&initialize()).unwrap())
    }));
    let mut stalled = post(None, initialize());
    *stalled.body_mut() = body;
    let ingress_service = service.clone();
    let ingress = tokio::spawn(async move { ingress_service.serve(stalled).await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), started_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.semaphore.available_permits(), 0);

    let polls = Arc::new(AtomicUsize::new(0));
    let mut saturated = post(None, initialize());
    *saturated.body_mut() = counting_body(&polls);
    assert_eq!(
        service.serve(saturated).await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    let trigger = Request::builder()
        .method("POST")
        .uri("/api/trigger")
        .header("authorization", "Bearer test-token")
        .header("content-type", "application/json")
        .body(counting_body(&polls))
        .unwrap();
    assert_eq!(
        service.serve(trigger).await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    for (path, expected) in [
        ("/api/health", StatusCode::OK),
        ("/monitor", StatusCode::OK),
        ("/api/trigger", StatusCode::UNAUTHORIZED),
    ] {
        assert_eq!(
            service
                .serve(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            expected
        );
    }
    let mut get = request("GET", Some("test-token"), Body::empty());
    get.headers_mut()
        .insert("mcp-session-id", session.parse().unwrap());
    get.headers_mut()
        .insert("accept", "text/event-stream".parse().unwrap());
    let stream = service.serve(get).await.unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    drop(stream);
    let mut delete = request("DELETE", Some("test-token"), Body::empty());
    delete
        .headers_mut()
        .insert("mcp-session-id", session.parse().unwrap());
    assert_eq!(
        service.serve(delete).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );

    resume_tx.send(()).unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), ingress)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.semaphore.available_permits(), 1);
    answer(response).await;
    state.shutdown_sessions().await;
}

#[tokio::test]
async fn origins_preflight_body_and_protocol_admission_stay_with_sdk() {
    let (_home, state) = mount().await;
    let endpoint = state.mcp.as_ref().unwrap();
    let polls = Arc::new(AtomicUsize::new(0));
    for method in ["OPTIONS", "POST", "GET", "DELETE"] {
        let mut denied = request(method, Some("test-token"), counting_body(&polls));
        denied
            .headers_mut()
            .insert("origin", "https://untrusted.example".parse().unwrap());
        assert_eq!(
            endpoint.handle(denied).await.status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    let mut preflight = request("OPTIONS", None, Body::empty());
    preflight
        .headers_mut()
        .insert("origin", "http://localhost:8080".parse().unwrap());
    preflight
        .headers_mut()
        .insert("access-control-request-method", "POST".parse().unwrap());
    let response = endpoint.handle(preflight).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        "http://localhost:8080"
    );
    let mut bad_media = post(None, initialize());
    bad_media
        .headers_mut()
        .insert("content-type", "text/plain".parse().unwrap());
    assert_eq!(
        endpoint.handle(bad_media).await.status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    let mut bad_accept = post(None, initialize());
    bad_accept
        .headers_mut()
        .insert("accept", "application/json".parse().unwrap());
    assert_eq!(
        endpoint.handle(bad_accept).await.status(),
        StatusCode::NOT_ACCEPTABLE
    );
    let mut oversized = post(None, initialize());
    *oversized.body_mut() = Body::from("x".repeat(4097));
    assert_eq!(
        endpoint.handle(oversized).await.status(),
        StatusCode::BAD_REQUEST
    );
    state.shutdown_sessions().await;
}

#[tokio::test]
async fn authenticated_factory_lists_tools_resources_tasks_and_independent_sessions() {
    let (_home, state) = mount().await;
    let service = crate::api::http_service(state.clone());
    let mut ids = Vec::new();
    for _ in 0..2 {
        let response = service.serve(post(None, initialize())).await.unwrap();
        let session = response.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let initialized = answer(response).await;
        assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
        assert!(initialized["result"]["capabilities"]["tasks"].is_object());
        let response = service
            .serve(post(
                Some(&session),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        for (method, key) in [
            ("tools/list", "tools"),
            ("resources/list", "resources"),
            ("tasks/list", "tasks"),
        ] {
            let response = service
                .serve(post(
                    Some(&session),
                    json!({"jsonrpc":"2.0","id":2,"method":method}),
                ))
                .await
                .unwrap();
            let result = answer(response).await;
            assert!(result.get("error").is_none(), "{result}");
            assert!(result["result"][key].is_array(), "{result}");
            if key != "tasks" {
                assert!(!result["result"][key].as_array().unwrap().is_empty());
            }
        }
        ids.push(session);
    }
    assert_ne!(ids[0], ids[1]);
    let endpoint = state.mcp.as_ref().unwrap();
    let sessions = endpoint.sessions.clone();
    assert_eq!(sessions.session_count(), 2);
    for session in sessions.sessions() {
        assert_eq!(session.owner.unwrap().subject, "chaos-operator");
    }
    let mut delete = request("DELETE", Some("test-token"), Body::empty());
    delete
        .headers_mut()
        .insert("mcp-session-id", ids[0].parse().unwrap());
    assert_eq!(
        service.serve(delete).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(sessions.session_count(), 1);
    let response = service
        .serve(post(
            Some(&ids[1]),
            json!({"jsonrpc":"2.0","id":3,"method":"tasks/list"}),
        ))
        .await
        .unwrap();
    assert!(answer(response).await["result"]["tasks"].is_array());
    state.shutdown_sessions().await;
    assert_eq!(sessions.session_count(), 0);
    assert_eq!(
        service
            .serve(post(None, initialize()))
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn sdk_pins_principal_to_session_and_explicit_nonlocal_origin() {
    // Production has one operator credential, not a multi-user product. This
    // fixture proves the delegated SDK path still enforces principal pinning.
    let mut config = transport_config(&cli(&["--mcp", "--bind", "0.0.0.0"]), "test-token").unwrap();
    config.allowed_origins = vec!["https://operator.example".to_owned()];
    config.bearer_auth = Some(
        HttpBearerAuthSet::new([
            HttpBearerAuthConfig::new("test-token", "owner").unwrap(),
            HttpBearerAuthConfig::new("other-token", "other").unwrap(),
        ])
        .unwrap(),
    );
    let service = McpHttpService::new(
        Arc::new(mcp_host::Server::builder("pinning-test", "1").build()),
        config,
    )
    .unwrap();
    let response = service.handle(post(None, initialize())).await;
    let session = response.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    answer(response).await;
    for method in ["GET", "DELETE", "POST"] {
        let mut wrong_owner = if method == "POST" {
            post(
                Some(&session),
                json!({"jsonrpc":"2.0","id":2,"method":"ping"}),
            )
        } else {
            let mut request = request(method, None, Body::empty());
            request
                .headers_mut()
                .insert("mcp-session-id", session.parse().unwrap());
            request
        };
        wrong_owner
            .headers_mut()
            .insert("authorization", "Bearer other-token".parse().unwrap());
        if method == "GET" {
            wrong_owner
                .headers_mut()
                .insert("accept", "text/event-stream".parse().unwrap());
        }
        assert_eq!(
            service.handle(wrong_owner).await.status(),
            StatusCode::FORBIDDEN
        );
    }
    for (origin, expected) in [
        ("https://operator.example", StatusCode::NO_CONTENT),
        ("http://localhost:8080", StatusCode::FORBIDDEN),
    ] {
        let mut preflight = request("OPTIONS", None, Body::empty());
        preflight
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
        assert_eq!(service.handle(preflight).await.status(), expected);
    }
}

#[tokio::test]
async fn sdk_bounds_sessions_and_streams_without_treating_disconnect_as_delete() {
    let config = transport_config(
        &cli(&["--mcp", "--max-concurrent", "1", "--max-sessions", "1"]),
        "test-token",
    )
    .unwrap();
    let service = McpHttpService::new(
        Arc::new(mcp_host::Server::builder("bounded-test", "1").build()),
        config,
    )
    .unwrap();
    let response = service.handle(post(None, initialize())).await;
    let session = response.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    answer(response).await;
    let get = || {
        let mut request = request("GET", Some("test-token"), Body::empty());
        request
            .headers_mut()
            .insert("accept", "text/event-stream".parse().unwrap());
        request
            .headers_mut()
            .insert("mcp-session-id", session.parse().unwrap());
        request
    };
    let stream = service.handle(get()).await;
    assert_eq!(stream.status(), StatusCode::OK);
    assert_eq!(
        service.handle(get()).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        service.handle(post(None, initialize())).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(stream);
    assert_eq!(service.session_handle().session_count(), 1);
    let reconnected = service.handle(get()).await;
    assert_eq!(reconnected.status(), StatusCode::OK);
}
