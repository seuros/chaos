use super::*;
use std::time::Duration;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn encoded_payload_is_shared_by_request_bodies() {
    let payload = serde_json::json!({"input": "x".repeat(1024 * 1024)});
    let encoded = encode_body(&payload).unwrap();
    let pointer = encoded.as_ptr();

    for _ in 0..3 {
        let body = Body::from(encoded.clone());
        let collected = body.collect().await.unwrap().to_bytes();
        assert_eq!(collected.as_ptr(), pointer);
        assert_eq!(collected.len(), encoded.len());
    }
    assert_eq!(serde_json::from_slice::<Value>(&encoded).unwrap(), payload);
}

#[tokio::test]
async fn retries_send_identical_payload_and_headers() {
    let server = MockServer::start().await;
    let payload = serde_json::json!({"input": "text\n\"escaped\"", "stream": true});
    for (priority, status) in [(1, 429), (2, 500), (3, 200)] {
        Mock::given(method("POST"))
            .and(path("/stream"))
            .and(header("content-type", "application/json"))
            .and(header("authorization", "Bearer test-token"))
            .and(body_json(&payload))
            .respond_with(ResponseTemplate::new(status))
            .with_priority(priority)
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
    }

    let mut provider =
        Provider::from_base_url_with_default_streaming_config("test", server.uri(), true);
    provider.retry.max_attempts = 3;
    provider.retry.base_delay = Duration::ZERO;
    let mut headers = HeaderMap::new();
    headers.insert(
        rama::http::header::AUTHORIZATION,
        rama::http::HeaderValue::from_static("Bearer test-token"),
    );
    headers.insert(
        rama::http::header::CONTENT_TYPE,
        rama::http::HeaderValue::from_static("application/json"),
    );

    let response = start_rama_post_sse_request(
        &format!("{}/stream", server.uri()),
        &headers,
        &payload,
        &provider,
        "test",
        None,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| request.body == requests[0].body)
    );
}

#[tokio::test]
async fn retry_limit_and_disabled_retries_are_preserved() {
    for (max_attempts, retry_5xx, expected_attempts) in [(2, true, 2), (3, false, 1), (0, true, 1)]
    {
        let server = MockServer::start().await;
        let payload = serde_json::json!({"input": "retry limit"});
        Mock::given(method("POST"))
            .and(body_json(&payload))
            .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
            .expect(expected_attempts)
            .mount(&server)
            .await;
        let mut provider =
            Provider::from_base_url_with_default_streaming_config("test", server.uri(), true);
        provider.retry.max_attempts = max_attempts;
        provider.retry.retry_5xx = retry_5xx;
        provider.retry.base_delay = Duration::ZERO;
        let result = start_rama_post_sse_request(
            &server.uri(),
            &HeaderMap::new(),
            &payload,
            &provider,
            "test",
            None,
        )
        .await;
        std::assert_matches!(result, Err(AbiError::Transport { status: 503, message })
            if message == "unavailable");
        assert_eq!(
            server.received_requests().await.unwrap().len() as u64,
            expected_attempts
        );
    }
}
