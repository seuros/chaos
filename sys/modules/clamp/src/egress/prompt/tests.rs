#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

const CLOUD_CODE: &str = "cloudcode-pa.googleapis.com";
const DAILY_CLOUD_CODE: &str = "daily-cloudcode-pa.googleapis.com";
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
    for host in [
        CLOUD_CODE,
        DAILY_CLOUD_CODE,
        "Daily-CloudCode-PA.googleapis.com.",
    ] {
        for path in ["/v1internal:generateContent", GENERATE] {
            let text = "Canonical instructions.\nExact whitespace:  é\n";
            let prompt = AntigravitySystemPrompt::new(text.to_string());
            let original = cloud_request();
            let mut req = generation_request(path, original.clone());
            req.headers_mut()
                .insert("digest", HeaderValue::from_static("old-body-digest"));
            req.headers_mut()
                .insert(TRANSFER_ENCODING, HeaderValue::from_static("chunked"));

            let rewritten = prompt.rewrite(req, host).await.unwrap();
            assert_eq!(rewritten.uri().request_target(), path);
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
    }
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
        (DAILY_CLOUD_CODE, "/v1internal:loadCodeAssist"),
        (DAILY_CLOUD_CODE, "/v1internal:fetchAvailableModels"),
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
        (DAILY_CLOUD_CODE, "/v2internal:streamGenerateContent"),
        ("daily-cloudcode-pa.googleapis.com.evil.test", GENERATE),
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

fn sample_audio_attachment(nonce: &str) -> AntigravityAudioAttachment {
    AntigravityAudioAttachment {
        nonce: nonce.to_string(),
        mime_type: "audio/wav".to_string(),
        data_base64: "UklGRiQAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQAAAAA=".to_string(),
    }
}

fn cloud_request_with_user_text(model: &str, user_text: &str) -> Value {
    json!({
        "model": model,
        "project": "test-project",
        "request": {
            "contents": [{"role": "user", "parts": [{"text": user_text}]}],
            "systemInstruction": {"parts": [{"text": "CLI agent instructions"}]},
            "tools": [{"functionDeclarations": [{"name": "chaos_tool"}]}],
            "generationConfig": {"temperature": 0.2}
        }
    })
}

#[tokio::test]
async fn armed_audio_splices_inline_data_beside_permanent_placeholder() {
    let prompt = AntigravitySystemPrompt::new("ChaOS instructions".to_string());
    let attachment = sample_audio_attachment("7f3a9c12");
    let guard = prompt
        .arm_audio("gemini-4-neon-high".to_string(), vec![attachment.clone()])
        .unwrap();

    let user_text = "<USER_REQUEST>\nWhich tone came first?\n[audio clip: tones.wav, 1.0 s, id 7f3a9c12]\n</USER_REQUEST>";
    let req = generation_request(
        GENERATE,
        cloud_request_with_user_text("gemini-4-neon-high", user_text),
    );
    let rewritten = prompt.rewrite(req, DAILY_CLOUD_CODE).await.unwrap();
    let content_length: usize = rewritten.headers()[CONTENT_LENGTH]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let bytes = rewritten.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(content_length, bytes.len());

    let actual: Value = serde_json::from_slice(&bytes).unwrap();
    let parts = actual["request"]["contents"][0]["parts"]
        .as_array()
        .unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["text"], user_text);
    assert_eq!(
        parts[1],
        json!({
            "inlineData": {
                "mimeType": "audio/wav",
                "data": attachment.data_base64,
            }
        })
    );
    assert_eq!(guard.disarm().unwrap(), 1);
    assert!(!format!("{attachment:?}").contains(&attachment.data_base64));
}

#[tokio::test]
async fn non_matching_auxiliary_and_history_echo_requests_never_splice_audio() {
    let prompt = AntigravitySystemPrompt::new("ChaOS instructions".to_string());
    let user_text = "[audio clip: tones.wav, 1.0 s, id deadbeef]";

    // 1. Unarmed turn: placeholder in text never triggers splice.
    let unarmed = payload(
        prompt
            .rewrite(
                generation_request(
                    GENERATE,
                    cloud_request_with_user_text("gemini-4-neon-high", user_text),
                ),
                CLOUD_CODE,
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        unarmed["request"]["contents"][0]["parts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Arm for gemini-4-neon-high.
    let guard = prompt
        .arm_audio(
            "gemini-4-neon-high".to_string(),
            vec![sample_audio_attachment("deadbeef")],
        )
        .unwrap();

    // 2. Different model (auxiliary model) with matching nonce -> no splice.
    let wrong_model = payload(
        prompt
            .rewrite(
                generation_request(
                    GENERATE,
                    cloud_request_with_user_text("gemini-4-flash-low", user_text),
                ),
                CLOUD_CODE,
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        wrong_model["request"]["contents"][0]["parts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // 3. Auxiliary call on the SAME model (no tools) copying user text + nonce -> no splice.
    let mut same_model_aux = cloud_request_with_user_text("gemini-4-neon-high", user_text);
    same_model_aux["request"]
        .as_object_mut()
        .unwrap()
        .remove("tools");
    let aux_resp = payload(
        prompt
            .rewrite(generation_request(GENERATE, same_model_aux), CLOUD_CODE)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        aux_resp["request"]["contents"][0]["parts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // 4. Placeholder echoed only in a role="model" part -> no splice.
    let echo_req = json!({
        "model": "gemini-4-neon-high",
        "project": "test-project",
        "request": {
            "contents": [
                {"role": "user", "parts": [{"text": "plain turn"}]},
                {"role": "model", "parts": [{"text": user_text}]}
            ],
            "tools": [{"functionDeclarations": [{"name": "chaos_tool"}]}]
        }
    });
    let echo_resp = payload(
        prompt
            .rewrite(generation_request(GENERATE, echo_req), CLOUD_CODE)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        echo_resp["request"]["contents"][0]["parts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        echo_resp["request"]["contents"][1]["parts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // None of those non-matching requests counted as a delivery.
    assert_eq!(guard.disarm().unwrap(), 0);
}

#[tokio::test]
async fn multi_step_turn_splices_every_matching_request_and_clears_on_disarm() {
    let prompt = AntigravitySystemPrompt::new("ChaOS instructions".to_string());
    let guard = prompt
        .arm_audio(
            "gemini-4-neon-high".to_string(),
            vec![sample_audio_attachment("abc12345")],
        )
        .unwrap();

    let user_text = "[audio clip: tones.wav, 1.0 s, id abc12345]";
    // Step 1: initial generation request.
    let step1 = payload(
        prompt
            .rewrite(
                generation_request(
                    GENERATE,
                    cloud_request_with_user_text("gemini-4-neon-high", user_text),
                ),
                DAILY_CLOUD_CODE,
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        step1["request"]["contents"][0]["parts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Step 2: post-tool-call request in the same turn resends contents[0].
    let step2_payload = json!({
        "model": "gemini-4-neon-high",
        "project": "test-project",
        "request": {
            "contents": [
                {"role": "user", "parts": [{"text": user_text}]},
                {"role": "model", "parts": [{"text": "calling tool"}]},
                {"role": "user", "parts": [{"text": "tool result"}]}
            ],
            "tools": [{"functionDeclarations": [{"name": "chaos_tool"}]}]
        }
    });
    let step2 = payload(
        prompt
            .rewrite(
                generation_request(GENERATE, step2_payload),
                DAILY_CLOUD_CODE,
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        step2["request"]["contents"][0]["parts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        step2["request"]["contents"][2]["parts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    assert_eq!(guard.disarm().unwrap(), 2);

    // Resumed turn after disarm: same placeholder text receives NO audio.
    let resumed = payload(
        prompt
            .rewrite(
                generation_request(
                    GENERATE,
                    cloud_request_with_user_text("gemini-4-neon-high", user_text),
                ),
                DAILY_CLOUD_CODE,
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        resumed["request"]["contents"][0]["parts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn ambiguous_user_target_and_oversized_serialized_request_fail_loud() {
    let prompt = AntigravitySystemPrompt::new("ChaOS instructions".to_string());
    let user_text = "[audio clip: tones.wav, 1.0 s, id dup99999]";

    // Ambiguous target: two role="user" messages contain the same nonce.
    let guard = prompt
        .arm_audio(
            "gemini-4-neon-high".to_string(),
            vec![sample_audio_attachment("dup99999")],
        )
        .unwrap();
    let ambiguous = json!({
        "model": "gemini-4-neon-high",
        "project": "test-project",
        "request": {
            "contents": [
                {"role": "user", "parts": [{"text": user_text}]},
                {"role": "user", "parts": [{"text": user_text}]}
            ],
            "tools": [{"functionDeclarations": [{"name": "chaos_tool"}]}]
        }
    });
    assert!(
        prompt
            .rewrite(generation_request(GENERATE, ambiguous), CLOUD_CODE)
            .await
            .is_err()
    );
    assert_eq!(guard.disarm().unwrap(), 0);

    // Oversized final serialized request (after splicing base64 audio) -> rejected.
    let guard = prompt
        .arm_audio(
            "gemini-4-neon-high".to_string(),
            vec![AntigravityAudioAttachment {
                nonce: "big12345".to_string(),
                mime_type: "audio/wav".to_string(),
                data_base64: "A".repeat(MAX_GENERATION_REQUEST_BYTES),
            }],
        )
        .unwrap();
    let oversized = cloud_request_with_user_text(
        "gemini-4-neon-high",
        "[audio clip: big.wav, 10.0 s, id big12345]",
    );
    assert!(
        prompt
            .rewrite(generation_request(GENERATE, oversized), CLOUD_CODE)
            .await
            .is_err()
    );
    assert_eq!(guard.disarm().unwrap(), 0);
}
