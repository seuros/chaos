use super::*;

#[test]
fn openai_turn_preparation_does_not_apply_grok_subscription_policy() {
    let adapter = OpenAiAdapter::from_base_url_and_api_key(
        "https://cli-chat-proxy.grok.com/v1".to_string(),
        "fixture-token".to_string(),
        Some("grok-4.3".to_string()),
    );
    let mut request = TurnRequest {
        model: String::new(),
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
    let options = adapter.prepare_turn(&mut request);
    assert_eq!(request.model, "grok-4.3");
    assert!(options.extra_headers.is_empty());
}

#[test]
fn web_search_content_types_follow_endpoint_compatibility() {
    use serde_json::json;

    for (endpoint, text_only) in [
        (chaos_services::openai::CHATGPT_BACKEND_BASE, true),
        ("https://chatgpt.com/backend-api/codex/", true),
        (chaos_services::openai::OPENAI_API_BASE, false),
        ("https://example.openai.azure.com/openai/v1", false),
        ("https://api.x.ai/v1", false),
        ("https://cli-chat-proxy.grok.com/v1", false),
        ("https://proxy.example/backend-api/codex", false),
        ("https://chatgpt.com.example/backend-api/codex", false),
        ("https://chatgpt.com@proxy.example/backend-api/codex", false),
        ("https://chatgpt.com/backend-api/codex-other", false),
        ("http://chatgpt.com/backend-api/codex", false),
    ] {
        let adapter = OpenAiAdapter::from_base_url_and_api_key(
            endpoint.to_string(),
            "fixture-token".to_string(),
            Some("fixture-model".to_string()),
        );
        // Both live and cached searches must retain their access policy.
        for external_web_access in [false, true] {
            let tools = json!([
                {
                    "type": "web_search",
                    "search_content_types": ["text", "image"],
                    "external_web_access": external_web_access,
                    "search_context_size": "high",
                    "filters": {"allowed_domains": ["example.com"]},
                    "user_location": {"type": "approximate", "country": "GB"}
                },
                {"type": "web_search", "external_web_access": external_web_access},
                {"type": "image_generation"},
                {
                    "type": "function",
                    "name": "web_search",
                    "parameters": {"type": "object"},
                    "search_content_types": ["image"]
                }
            ]);
            let mut request = TurnRequest {
                model: String::new(),
                instructions: String::new(),
                input: vec![],
                tools: vec![],
                parallel_tool_calls: false,
                reasoning: None,
                output_schema: None,
                verbosity: None,
                turn_state: None,
                extensions: serde_json::Map::from_iter([(
                    "openai_tools".to_string(),
                    tools.clone(),
                )]),
            };
            adapter.prepare_turn(&mut request);
            // Both HTTP and WebSocket transports consume this same projection.
            let wire = crate::adapter::turn_request_to_api_request(
                request,
                crate::representer::SessionRepresenter::openai().as_representer(),
            );
            let mut expected = tools;
            if text_only {
                expected[0]
                    .as_object_mut()
                    .unwrap()
                    .remove("search_content_types");
            }
            assert_eq!(json!(wire.tools), expected, "{endpoint}");
        }
    }
}

#[test]
fn static_auth_debug_redacts_token() {
    let auth = StaticAuthProvider::new(
        Some("fixture-token".to_string()),
        Some("account".to_string()),
    );
    assert_eq!(auth.bearer_token().as_deref(), Some("fixture-token"));
    assert_eq!(auth.account_id().as_deref(), Some("account"));
    assert!(!format!("{auth:?}").contains("fixture-token"));
}
