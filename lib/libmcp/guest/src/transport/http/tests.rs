use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use rama::bytes::Bytes;
use rama::service::service_fn;
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;

use super::*;
use crate::protocol::ClientCapabilities;
use crate::protocol::Implementation;
use crate::protocol::InitializeRequest;
use crate::protocol::JsonRpcRequest;
use crate::protocol::ServerCapabilities;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SeenRequest {
    http_method: String,
    rpc_method: Option<String>,
    session_id: Option<String>,
}

async fn record_request(req: Request) -> SeenRequest {
    let http_method = req.method().as_str().to_string();
    let session_id = req
        .headers()
        .get(HEADER_SESSION_ID)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let rpc_method = if http_method == "POST" {
        let body = req.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(body.as_ref()).unwrap();
        value
            .get("method")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned)
    } else {
        None
    };

    SeenRequest {
        http_method,
        rpc_method,
        session_id,
    }
}

fn initialize_http_response(session_id: &str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, MIME_APPLICATION_JSON)
        .header(HEADER_SESSION_ID, session_id)
        .body(Body::from(
            serde_json::to_vec(&initialize_response()).unwrap(),
        ))
        .unwrap()
}

fn initialize_response() -> JsonRpcMessage {
    JsonRpcMessage::Response(JsonRpcResponse::success(
        json!(1),
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": ServerCapabilities::default(),
            "serverInfo": {
                "name": "test-server",
                "version": "0.1.0"
            }
        }),
    ))
}

#[tokio::test]
async fn transport_posts_initialize_response() {
    let response = serde_json::to_vec(&initialize_response()).unwrap();
    let client = service_fn(move |_req: Request| {
        let response = response.clone();
        async move {
            Ok::<_, OpaqueError>(
                Response::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_TYPE, MIME_APPLICATION_JSON)
                    .header(HEADER_SESSION_ID, "session-123")
                    .body(Body::from(response))
                    .unwrap(),
            )
        }
    })
    .boxed();

    let transport = HttpTransport::with_client(
        HttpClientConfig {
            endpoint: Url::parse("http://localhost:62770/mcp").unwrap(),
            open_sse_stream: false,
            reconnect_delay: Duration::from_millis(5),
            default_headers: Vec::new(),
        },
        client,
    );

    transport
        .send(JsonRpcMessage::Request(JsonRpcRequest::new(
            json!(1),
            "initialize",
            Some(
                serde_json::to_value(InitializeRequest {
                    protocol_version: "2025-11-25".to_string(),
                    capabilities: ClientCapabilities::default(),
                    client_info: Implementation::new("mcp-guest", "0.1.0"),
                })
                .unwrap(),
            ),
        )))
        .await
        .unwrap();

    let message = transport.recv().await.unwrap();
    let JsonRpcMessage::Response(response) = message else {
        panic!("expected initialize response");
    };
    assert_eq!(response.id, Some(json!(1)));
    assert!(response.error.is_none());
    let result = response.result.expect("initialize result");
    assert_eq!(result["protocolVersion"], json!("2025-11-25"));
    assert_eq!(result["serverInfo"]["name"], json!("test-server"));
    assert_eq!(
        transport.inner.session_id.lock().await.as_deref(),
        Some("session-123")
    );
    assert_eq!(
        transport.inner.negotiated_version.lock().await.as_deref(),
        Some("2025-11-25")
    );
}

#[tokio::test]
async fn transport_reads_get_sse_notifications() {
    let get_count = Arc::new(AtomicUsize::new(0));
    let client = {
        let get_count = Arc::clone(&get_count);
        service_fn(move |req: Request| {
            let get_count = Arc::clone(&get_count);
            async move {
                let response = match req.method().as_str() {
                    "POST" => match record_request(req).await.rpc_method.as_deref() {
                        Some("initialize") => initialize_http_response("session-123"),
                        Some("notifications/initialized") => Response::builder()
                            .status(StatusCode::ACCEPTED)
                            .body(Body::empty())
                            .unwrap(),
                        other => Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::from(format!("unexpected rpc method {other:?}")))
                            .unwrap(),
                    },
                    "GET" => {
                        if get_count.fetch_add(1, Ordering::Relaxed) == 0 {
                            let body = Body::from_stream(tokio_stream::iter(vec![Ok::<_, OpaqueError>(
                                Bytes::from(
                                    "id: session-123-1\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/resources/list_changed\"}\n\n",
                                ),
                            )]));
                            Response::builder()
                                .status(StatusCode::OK)
                                .header(CONTENT_TYPE, MIME_TEXT_EVENT_STREAM)
                                .body(body)
                                .unwrap()
                        } else {
                            Response::builder()
                                .status(StatusCode::METHOD_NOT_ALLOWED)
                                .body(Body::empty())
                                .unwrap()
                        }
                    }
                    method => Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::from(method.to_string()))
                        .unwrap(),
                };

                Ok::<_, OpaqueError>(response)
            }
        })
        .boxed()
    };

    let transport = HttpTransport::with_client(
        HttpClientConfig {
            endpoint: Url::parse("http://localhost:62770/mcp").unwrap(),
            open_sse_stream: true,
            reconnect_delay: Duration::from_millis(5),
            default_headers: Vec::new(),
        },
        client,
    );

    transport
        .send(JsonRpcMessage::Request(JsonRpcRequest::new(
            json!(1),
            "initialize",
            Some(
                serde_json::to_value(InitializeRequest {
                    protocol_version: "2025-11-25".to_string(),
                    capabilities: ClientCapabilities::default(),
                    client_info: Implementation::new("mcp-guest", "0.1.0"),
                })
                .unwrap(),
            ),
        )))
        .await
        .unwrap();

    let first = transport.recv().await.unwrap();
    let JsonRpcMessage::Response(response) = first else {
        panic!("expected initialize response");
    };
    assert_eq!(response.id, Some(json!(1)));
    assert!(response.error.is_none());

    transport
        .send(JsonRpcMessage::Notification(JsonRpcRequest::notification(
            "notifications/initialized",
            None,
        )))
        .await
        .unwrap();

    let second = tokio::time::timeout(Duration::from_secs(1), transport.recv())
        .await
        .unwrap()
        .unwrap();
    let JsonRpcMessage::Notification(notification) = second else {
        panic!("expected resources/list_changed notification");
    };
    assert_eq!(notification.method, "notifications/resources/list_changed");
    assert!(notification.params.is_none());
}

#[tokio::test]
async fn transport_recovers_session_on_404_and_retries_request() {
    let seen_requests = Arc::new(AsyncMutex::new(Vec::<SeenRequest>::new()));
    let call_count = Arc::new(AtomicUsize::new(0));

    let client = {
        let seen_requests = Arc::clone(&seen_requests);
        let call_count = Arc::clone(&call_count);
        service_fn(move |req: Request| {
            let seen_requests = Arc::clone(&seen_requests);
            let call_count = Arc::clone(&call_count);
            async move {
                let seen = record_request(req).await;
                seen_requests.lock().await.push(seen);

                let response = match call_count.fetch_add(1, Ordering::Relaxed) {
                    0 => initialize_http_response("session-1"),
                    1 => Response::builder()
                        .status(StatusCode::ACCEPTED)
                        .body(Body::empty())
                        .unwrap(),
                    2 => Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(Body::empty())
                        .unwrap(),
                    3 => initialize_http_response("session-2"),
                    4 => Response::builder()
                        .status(StatusCode::ACCEPTED)
                        .header(HEADER_SESSION_ID, "session-2")
                        .body(Body::empty())
                        .unwrap(),
                    5 => Response::builder()
                        .status(StatusCode::OK)
                        .header(CONTENT_TYPE, MIME_APPLICATION_JSON)
                        .header(HEADER_SESSION_ID, "session-2")
                        .body(Body::from(
                            serde_json::to_vec(&JsonRpcMessage::Response(
                                JsonRpcResponse::success(json!(2), json!({})),
                            ))
                            .unwrap(),
                        ))
                        .unwrap(),
                    other => Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::from(format!("unexpected call {other}")))
                        .unwrap(),
                };

                Ok::<_, OpaqueError>(response)
            }
        })
        .boxed()
    };

    let transport = HttpTransport::with_client(
        HttpClientConfig {
            endpoint: Url::parse("http://localhost:62770/mcp").unwrap(),
            open_sse_stream: false,
            reconnect_delay: Duration::from_millis(5),
            default_headers: Vec::new(),
        },
        client,
    );

    transport
        .send(JsonRpcMessage::Request(JsonRpcRequest::new(
            json!(1),
            "initialize",
            Some(
                serde_json::to_value(InitializeRequest {
                    protocol_version: "2025-11-25".to_string(),
                    capabilities: ClientCapabilities::default(),
                    client_info: Implementation::new("mcp-guest", "0.1.0"),
                })
                .unwrap(),
            ),
        )))
        .await
        .unwrap();
    let _ = transport.recv().await.unwrap();

    transport
        .send(JsonRpcMessage::Notification(JsonRpcRequest::notification(
            "notifications/initialized",
            None,
        )))
        .await
        .unwrap();

    transport
        .send(JsonRpcMessage::Request(JsonRpcRequest::new(
            json!(2),
            "ping",
            Some(json!({})),
        )))
        .await
        .unwrap();

    let message = transport.recv().await.unwrap();
    let JsonRpcMessage::Response(response) = message else {
        panic!("expected retried ping response");
    };
    assert_eq!(response.id, Some(json!(2)));
    assert!(response.error.is_none());

    let seen = seen_requests.lock().await.clone();
    assert_eq!(
        seen,
        vec![
            SeenRequest {
                http_method: "POST".to_string(),
                rpc_method: Some("initialize".to_string()),
                session_id: None,
            },
            SeenRequest {
                http_method: "POST".to_string(),
                rpc_method: Some("notifications/initialized".to_string()),
                session_id: Some("session-1".to_string()),
            },
            SeenRequest {
                http_method: "POST".to_string(),
                rpc_method: Some("ping".to_string()),
                session_id: Some("session-1".to_string()),
            },
            SeenRequest {
                http_method: "POST".to_string(),
                rpc_method: Some("initialize".to_string()),
                session_id: None,
            },
            SeenRequest {
                http_method: "POST".to_string(),
                rpc_method: Some("notifications/initialized".to_string()),
                session_id: Some("session-2".to_string()),
            },
            SeenRequest {
                http_method: "POST".to_string(),
                rpc_method: Some("ping".to_string()),
                session_id: Some("session-2".to_string()),
            },
        ]
    );
}

#[tokio::test]
async fn transport_recovers_missing_session_without_double_executing_tool_call() {
    let seen_requests = Arc::new(AsyncMutex::new(Vec::<SeenRequest>::new()));
    let call_count = Arc::new(AtomicUsize::new(0));
    let executed_tool_calls = Arc::new(AtomicUsize::new(0));

    let client = {
        let seen_requests = Arc::clone(&seen_requests);
        let call_count = Arc::clone(&call_count);
        let executed_tool_calls = Arc::clone(&executed_tool_calls);
        service_fn(move |req: Request| {
            let seen_requests = Arc::clone(&seen_requests);
            let call_count = Arc::clone(&call_count);
            let executed_tool_calls = Arc::clone(&executed_tool_calls);
            async move {
                let seen = record_request(req).await;
                seen_requests.lock().await.push(seen);

                let response = match call_count.fetch_add(1, Ordering::Relaxed) {
                    0 => initialize_http_response("session-1"),
                    1 => Response::builder()
                        .status(StatusCode::ACCEPTED)
                        .body(Body::empty())
                        .unwrap(),
                    // This is the post-restart race: the SSE loop has already
                    // cleared the old session, so the request has no header.
                    // A conforming server rejects it before tool dispatch.
                    2 => Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Body::from(
                            json!({"error": "Missing Mcp-Session-Id header"}).to_string(),
                        ))
                        .unwrap(),
                    3 => initialize_http_response("session-2"),
                    4 => Response::builder()
                        .status(StatusCode::ACCEPTED)
                        .header(HEADER_SESSION_ID, "session-2")
                        .body(Body::empty())
                        .unwrap(),
                    5 => {
                        executed_tool_calls.fetch_add(1, Ordering::Relaxed);
                        Response::builder()
                            .status(StatusCode::OK)
                            .header(CONTENT_TYPE, MIME_APPLICATION_JSON)
                            .header(HEADER_SESSION_ID, "session-2")
                            .body(Body::from(
                                serde_json::to_vec(&JsonRpcMessage::Response(
                                    JsonRpcResponse::success(
                                        json!(2),
                                        json!({
                                            "content": [{"type": "text", "text": "ok"}]
                                        }),
                                    ),
                                ))
                                .unwrap(),
                            ))
                            .unwrap()
                    }
                    other => Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::from(format!("unexpected call {other}")))
                        .unwrap(),
                };

                Ok::<_, OpaqueError>(response)
            }
        })
        .boxed()
    };

    let transport = HttpTransport::with_client(
        HttpClientConfig {
            endpoint: Url::parse("http://localhost:62770/mcp").unwrap(),
            open_sse_stream: false,
            reconnect_delay: Duration::from_millis(5),
            default_headers: Vec::new(),
        },
        client,
    );

    transport
        .send(JsonRpcMessage::Request(JsonRpcRequest::new(
            json!(1),
            "initialize",
            Some(
                serde_json::to_value(InitializeRequest {
                    protocol_version: "2025-11-25".to_string(),
                    capabilities: ClientCapabilities::default(),
                    client_info: Implementation::new("mcp-guest", "0.1.0"),
                })
                .unwrap(),
            ),
        )))
        .await
        .unwrap();
    let _ = transport.recv().await.unwrap();

    transport
        .send(JsonRpcMessage::Notification(JsonRpcRequest::notification(
            "notifications/initialized",
            None,
        )))
        .await
        .unwrap();

    // Mirror the SSE restart race: the background GET observes the dead
    // session and clears local state before the foreground request starts.
    transport.inner.clear_session_state().await;

    transport
        .send(JsonRpcMessage::Request(JsonRpcRequest::new(
            json!(2),
            "tools/call",
            Some(json!({
                "name": "create_item",
                "arguments": {"name": "only-once"}
            })),
        )))
        .await
        .unwrap();

    let message = transport.recv().await.unwrap();
    let JsonRpcMessage::Response(response) = message else {
        panic!("expected retried tool response");
    };
    assert_eq!(response.id, Some(json!(2)));
    assert!(response.error.is_none());
    assert_eq!(executed_tool_calls.load(Ordering::Relaxed), 1);

    let seen = seen_requests.lock().await.clone();
    let tool_posts = seen
        .iter()
        .filter(|request| request.rpc_method.as_deref() == Some("tools/call"))
        .collect::<Vec<_>>();
    assert_eq!(tool_posts.len(), 2);
    assert_eq!(tool_posts[0].session_id, None);
    assert_eq!(tool_posts[1].session_id.as_deref(), Some("session-2"));
}

#[test]
fn generic_bad_request_is_not_treated_as_a_stale_session() {
    assert!(!is_stale_session_rejection(
        StatusCode::BAD_REQUEST,
        Some("invalid tool arguments")
    ));
    assert!(!is_stale_session_rejection(
        StatusCode::BAD_REQUEST,
        Some(r#"{"error":"invalid tool arguments"}"#)
    ));
    assert!(!is_stale_session_rejection(
        StatusCode::UNAUTHORIZED,
        Some("Missing Mcp-Session-Id header")
    ));
}

#[tokio::test]
async fn transport_applies_custom_headers_and_merges_accept() {
    let client = service_fn(|_req: Request| async move {
        Ok::<_, OpaqueError>(
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap(),
        )
    })
    .boxed();

    let transport = HttpTransport::with_client(
        HttpClientConfig {
            endpoint: Url::parse("http://localhost:62770/mcp").unwrap(),
            open_sse_stream: false,
            reconnect_delay: Duration::from_millis(5),
            default_headers: vec![
                ("Authorization".to_string(), "Bearer secret".to_string()),
                ("X-Test".to_string(), "1".to_string()),
                (
                    "Accept".to_string(),
                    "application/vnd.example+json".to_string(),
                ),
                ("Content-Type".to_string(), "text/plain".to_string()),
                (HEADER_SESSION_ID.to_string(), "ignored".to_string()),
            ],
        },
        client,
    );

    let post_request = transport
        .inner
        .build_post_request(
            &JsonRpcMessage::Request(JsonRpcRequest::new(json!(1), "initialize", None)),
            true,
        )
        .await
        .unwrap();

    assert_eq!(
        post_request.headers()["authorization"].to_str().unwrap(),
        "Bearer secret"
    );
    assert_eq!(post_request.headers()["x-test"].to_str().unwrap(), "1");
    assert_eq!(
        post_request.headers()[CONTENT_TYPE].to_str().unwrap(),
        MIME_APPLICATION_JSON
    );
    let post_accept = post_request.headers()[ACCEPT].to_str().unwrap();
    assert!(post_accept.contains(MIME_APPLICATION_JSON));
    assert!(post_accept.contains(MIME_TEXT_EVENT_STREAM));
    assert!(post_accept.contains("application/vnd.example+json"));
    assert!(post_request.headers().get(HEADER_SESSION_ID).is_none());

    *transport.inner.session_id.lock().await = Some("session-123".to_string());
    *transport.inner.negotiated_version.lock().await = Some("2025-11-25".to_string());
    *transport.inner.last_event_id.lock().await = Some("event-42".to_string());

    let get_request = transport.inner.build_get_request().await.unwrap();
    assert_eq!(
        get_request.headers()["authorization"].to_str().unwrap(),
        "Bearer secret"
    );
    assert_eq!(get_request.headers()["x-test"].to_str().unwrap(), "1");
    assert_eq!(
        get_request.headers()[HEADER_SESSION_ID].to_str().unwrap(),
        "session-123"
    );
    assert_eq!(
        get_request.headers()[HEADER_PROTOCOL_VERSION]
            .to_str()
            .unwrap(),
        "2025-11-25"
    );
    assert_eq!(
        get_request.headers()[HEADER_LAST_EVENT_ID]
            .to_str()
            .unwrap(),
        "event-42"
    );
    let get_accept = get_request.headers()[ACCEPT].to_str().unwrap();
    assert!(get_accept.contains(MIME_TEXT_EVENT_STREAM));
    assert!(get_accept.contains("application/vnd.example+json"));
}
