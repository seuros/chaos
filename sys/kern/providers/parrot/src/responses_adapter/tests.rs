use super::*;
use chaos_ipc::protocol::SessionSource;
use chaos_test_fixtures::TEST_MODEL;
use std::sync::OnceLock;

#[test]
fn model_discovery_keeps_only_models_that_can_answer_a_prompt() {
    // Named like chat models, and nothing declared: kept.
    for id in [
        "gpt-5.6-sol",
        "grok-4.5",
        "deepseek-reasoner",
        "mistral-large-latest",
        "pixtral-12b",
    ] {
        assert!(can_carry_a_turn(id, None), "{id} should be kept");
    }

    // Named like the families that ride along in the same listing.
    for id in [
        "mistral-embed",
        "codestral-embed",
        "mistral-ocr-latest",
        "mistral-moderation-latest",
        "voxtral-mini-latest-tts",
        "whisper-1",
        "gpt-image-1",
        "llama-guard-4",
    ] {
        assert!(!can_carry_a_turn(id, None), "{id} should be dropped");
    }

    // A declared capability outranks the name in both directions.
    assert!(can_carry_a_turn("mistral-ocr-latest", Some(true)));
    assert!(!can_carry_a_turn("mistral-large-latest", Some(false)));
}

/// The same rule applied to a real listing, in the shape Mistral actually
/// publishes: only the models a turn can be sent to survive, and what the
/// provider says about them is carried across rather than defaulted away.
#[tokio::test]
async fn discovery_drops_models_that_cannot_answer_a_prompt() {
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "object": "list",
            "data": [
                {
                    "id": "mistral-large-latest",
                    "name": "Mistral Large",
                    "description": "Top-tier reasoning model.",
                    "max_context_length": 262_144,
                    "capabilities": {
                        "completion_chat": true,
                        "vision": true,
                        "reasoning": true,
                    },
                },
                { "id": "mistral-embed", "capabilities": { "completion_chat": false } },
                { "id": "mistral-ocr-latest" },
                { "id": "voxtral-mini-latest-tts" },
                // A declaration outranks the name in both directions.
                { "id": "codestral-embed", "capabilities": { "completion_chat": true } },
                { "id": "chatty-mcchatface", "capabilities": { "completion_chat": false } },
            ]
        })))
        .mount(&server)
        .await;

    let models = fetch_models(&server.uri(), None, None, HeaderMap::new())
        .await
        .expect("listing succeeds");
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["mistral-large-latest", "codestral-embed"]);

    let large = &models[0];
    assert_eq!(large.display_name, "Mistral Large");
    assert_eq!(
        large.description.as_deref(),
        Some("Top-tier reasoning model.")
    );
    assert_eq!(large.max_input_tokens, Some(262_144));
    assert!(large.supports_images, "declared vision should be carried");
    assert!(
        large.supports_thinking,
        "declared reasoning should be carried"
    );
}

#[test]
fn responses_options_preserve_conversation_and_session_when_not_overridden() {
    let mut options = ResponsesOptions {
        conversation_id: Some("conv-123".to_string()),
        session_source: Some(SessionSource::Cli),
        ..ResponsesOptions::default()
    };
    options.extra_headers.insert(
        HeaderName::from_static("x-test"),
        HeaderValue::from_static("present"),
    );

    let request = TurnRequest {
        model: TEST_MODEL.to_string(),
        instructions: String::new(),
        input: vec![],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: serde_json::Map::new(),
    };

    let resolved = responses_options_from_turn_request(&request, options);

    assert_eq!(resolved.conversation_id.as_deref(), Some("conv-123"));
    assert_eq!(resolved.session_source, Some(SessionSource::Cli));
    assert_eq!(
        resolved
            .extra_headers
            .get("x-test")
            .and_then(|value| value.to_str().ok()),
        Some("present")
    );
}

#[test]
fn responses_options_apply_request_level_overrides() {
    let turn_state = Arc::new(OnceLock::new());
    let mut extensions = serde_json::Map::new();
    extensions.insert(
        "request_headers".to_string(),
        serde_json::json!({"x-test": "override"}),
    );
    extensions.insert("compression".to_string(), serde_json::json!("zstd"));
    let request = TurnRequest {
        model: TEST_MODEL.to_string(),
        instructions: String::new(),
        input: vec![],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: Some(turn_state.clone()),
        extensions,
    };

    let resolved = responses_options_from_turn_request(&request, ResponsesOptions::default());

    assert_eq!(
        resolved
            .extra_headers
            .get("x-test")
            .and_then(|value| value.to_str().ok()),
        Some("override")
    );
    std::assert_matches!(resolved.compression, Compression::Zstd);
    assert!(
        resolved
            .turn_state
            .as_ref()
            .is_some_and(|state| Arc::ptr_eq(state, &turn_state))
    );
}

#[test]
fn malformed_overrides_clear_defaults_and_invalid_header_entries_are_skipped() {
    let mut options = ResponsesOptions {
        compression: Compression::Zstd,
        ..ResponsesOptions::default()
    };
    options.extra_headers.insert(
        HeaderName::from_static("x-default"),
        HeaderValue::from_static("present"),
    );
    let mut request = TurnRequest {
        model: TEST_MODEL.to_string(),
        instructions: String::new(),
        input: vec![],
        tools: vec![],
        parallel_tool_calls: false,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: serde_json::Map::new(),
    };
    request
        .extensions
        .insert("request_headers".to_string(), Value::Null);
    request
        .extensions
        .insert("compression".to_string(), Value::Null);
    let resolved = responses_options_from_turn_request(&request, options);
    assert!(resolved.extra_headers.is_empty());
    std::assert_matches!(resolved.compression, Compression::None);

    let headers = parse_request_headers(Some(&serde_json::json!({
        "x-valid": "present",
        "invalid header name": "ignored",
        "x-invalid-value": "line\nbreak",
        "x-not-a-string": 7,
    })));
    assert_eq!(headers.len(), 1);
    assert_eq!(headers["x-valid"], "present");
}

#[test]
fn neutral_discovery_never_invents_images_or_native_tools() {
    let models = models_from_slice(
        br#"{"data":[
            {"id":"grok-4.6"},
            {"id":"grok-4.5","supports_vision":true,"capabilities":{"vision":false}},
            {"id":"gpt-5.6","max_output_tokens":1024,"supports_thinking":true},
            {"id":"compat-chat","supports_image_input":true}
        ]}"#,
    )
    .expect("fixture listing");
    assert!(!models[0].supports_images);
    assert!(
        !models[1].supports_images,
        "capability block overrides hint"
    );
    assert_eq!(models[2].max_output_tokens, Some(1024));
    assert!(models[2].supports_thinking);
    assert!(models[3].supports_images);
    assert!(
        models
            .iter()
            .all(|model| model.native_server_side_tools.is_empty())
    );
}
