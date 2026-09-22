#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

const CLOUD_CODE: &str = "cloudcode-pa.googleapis.com";
const GENERATE: &str = "/v1internal:streamGenerateContent?alt=sse";

fn generation_request(path: &str, payload: Value) -> Request {
    let bytes = serde_json::to_vec(&payload).unwrap();
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .header(CONTENT_LENGTH, bytes.len().to_string())
        .header("authorization", "Bearer transport-owned-token")
        .body(Body::from(bytes))
        .unwrap()
}

fn cloud_request() -> Value {
    json!({
        "model": "test-model",
        "project": "test-project",
        "request": {
            "contents": [{"role": "user", "parts": [{"text": "operator request"}]}],
            "systemInstruction": {"parts": [{"text": "CLI agent instructions"}]},
            "tools": [{"functionDeclarations": [{"name": "chaos_tool"}]}],
            "generationConfig": {"temperature": 0.2}
        }
    })
}

async fn payload(req: Request) -> Value {
    let bytes = req.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn cloud_code_replaces_only_the_system_prompt_and_repairs_framing() {
    let text = "Canonical instructions.\nExact whitespace:  é\n";
    let prompt = AntigravitySystemPrompt::new(text.to_string());
    let original = cloud_request();
    let mut req = generation_request(GENERATE, original.clone());
    req.headers_mut()
        .insert("digest", HeaderValue::from_static("old-body-digest"));
    req.headers_mut()
        .insert(TRANSFER_ENCODING, HeaderValue::from_static("chunked"));

    let rewritten = prompt.rewrite(req, CLOUD_CODE).await.unwrap();
    assert_eq!(rewritten.uri().request_target(), GENERATE);
    assert_eq!(
        rewritten.headers()["authorization"],
        "Bearer transport-owned-token"
    );
    assert!(!rewritten.headers().contains_key("digest"));
    assert!(!rewritten.headers().contains_key(TRANSFER_ENCODING));
    let length: usize = rewritten.headers()[CONTENT_LENGTH]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let bytes = rewritten.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(length, bytes.len());

    let actual: Value = serde_json::from_slice(&bytes).unwrap();
    let mut expected = original;
    expected["request"]["systemInstruction"] = json!({"parts": [{"text": text}]});
    assert_eq!(actual, expected);
    assert!(!actual.to_string().contains("CLI agent instructions"));
    assert!(!actual["request"]["contents"].to_string().contains(text));
}

#[tokio::test]
async fn gemini_replaces_system_prompt_without_a_cloud_code_envelope() {
    let prompt = AntigravitySystemPrompt::new("ChaOS instructions".to_string());
    for path in [
        "/v1beta/models/test-model:generateContent",
        "/v1/models/test-model:streamGenerateContent?alt=sse",
    ] {
        let original = json!({
            "contents": [{"role": "user", "parts": [{"text": "operator request"}]}],
            "system_instruction": {"parts": [{"text": "old instructions"}]},
            "tools": []
        });
        let req = generation_request(path, original);
        let actual = payload(
            prompt
                .rewrite(req, "generativelanguage.googleapis.com")
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            actual["systemInstruction"],
            json!({"parts": [{"text": "ChaOS instructions"}]})
        );
        assert!(actual.get("system_instruction").is_none());
        assert_eq!(
            actual["contents"][0]["parts"][0]["text"],
            "operator request"
        );
    }
}

#[tokio::test]
async fn resumed_connections_use_updated_prompts_and_empty_means_no_cli_defaults() {
    let prompt = AntigravitySystemPrompt::new("first turn".to_string());
    let connection = prompt.clone();
    for text in [
        "first turn".to_string(),
        "updated é\n".repeat(5_000),
        String::new(),
    ] {
        prompt.update(text.clone()).unwrap();
        let mut original = cloud_request();
        original["request"]["system_instruction"] = json!({"parts": [{"text": "stale alias"}]});
        let actual = payload(
            connection
                .rewrite(generation_request(GENERATE, original), CLOUD_CODE)
                .await
                .unwrap(),
        )
        .await;
        assert!(actual["request"].get("system_instruction").is_none());
        if text.is_empty() {
            assert!(actual["request"].get("systemInstruction").is_none());
        } else {
            assert_eq!(
                actual["request"]["systemInstruction"]["parts"][0]["text"],
                text
            );
        }
    }
}

#[tokio::test]
async fn missing_system_field_is_populated_but_missing_generation_envelope_is_rejected() {
    let prompt = AntigravitySystemPrompt::new("canonical instructions".to_string());
    let original = json!({"request": {"contents": []}});
    let actual = payload(
        prompt
            .rewrite(generation_request(GENERATE, original), CLOUD_CODE)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        actual["request"]["systemInstruction"]["parts"][0]["text"],
        "canonical instructions"
    );

    for invalid in [
        json!({}),
        json!({"request": null}),
        json!({"request": {}}),
        json!({"request": {"contents": "not an array"}}),
    ] {
        assert!(
            prompt
                .rewrite(generation_request(GENERATE, invalid), CLOUD_CODE)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn oauth_and_control_requests_are_byte_identical_and_do_not_receive_the_prompt() {
    let prompt = AntigravitySystemPrompt::new("private base instructions".to_string());
    for (host, path) in [
        ("oauth2.googleapis.com", "/token"),
        (CLOUD_CODE, "/v1internal:loadCodeAssist"),
        (CLOUD_CODE, "/v1internal:fetchAvailableModels"),
    ] {
        let body = "grant_type=refresh_token&refresh_token=example";
        let req = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(CONTENT_LENGTH, body.len().to_string())
            .body(Body::from(body))
            .unwrap();
        let rewritten = prompt.rewrite(req, host).await.unwrap();
        assert_eq!(
            rewritten.headers()[CONTENT_TYPE],
            "application/x-www-form-urlencoded"
        );
        assert_eq!(
            rewritten.into_body().collect().await.unwrap().to_bytes(),
            body
        );
    }
    assert!(!format!("{prompt:?}").contains("private base instructions"));
}

#[tokio::test]
async fn unsupported_generation_endpoints_and_formats_fail_closed() {
    let prompt = AntigravitySystemPrompt::new("canonical instructions".to_string());
    for (host, path) in [
        (CLOUD_CODE, "/v2internal:streamGenerateContent"),
        (
            CLOUD_CODE,
            "/google.internal.cloud.code.v1internal.PredictionService/GenerateContent",
        ),
        (
            "generativelanguage.googleapis.com",
            "/v1beta/models/test-model:batchGenerateContent",
        ),
        (
            "generativelanguage.googleapis.com",
            "/v1beta/models/test-model:BidiGenerateContent",
        ),
        ("oauth2.googleapis.com", "/v1internal:generateContent"),
    ] {
        assert!(
            prompt
                .rewrite(generation_request(path, cloud_request()), host)
                .await
                .is_err()
        );
    }
    for (header, value) in [
        (CONTENT_ENCODING, "gzip"),
        (CONTENT_TYPE, "application/grpc"),
    ] {
        let mut req = generation_request(GENERATE, cloud_request());
        req.headers_mut()
            .insert(header, HeaderValue::from_static(value));
        assert!(prompt.rewrite(req, CLOUD_CODE).await.is_err());
    }
    let mut req = generation_request(GENERATE, cloud_request());
    *req.method_mut() = Method::GET;
    assert!(prompt.rewrite(req, CLOUD_CODE).await.is_err());

    for body in [
        "not JSON".to_string(),
        "x".repeat(MAX_GENERATION_REQUEST_BYTES + 1),
    ] {
        let mut req = generation_request(GENERATE, cloud_request());
        *req.body_mut() = Body::from(body);
        assert!(prompt.rewrite(req, CLOUD_CODE).await.is_err());
    }
    for cache_key in ["cachedContent", "cached_content"] {
        let mut original = cloud_request();
        original["request"][cache_key] = json!("cachedContents/old-system-prompt");
        assert!(
            prompt
                .rewrite(generation_request(GENERATE, original), CLOUD_CODE)
                .await
                .is_err()
        );
    }
}
