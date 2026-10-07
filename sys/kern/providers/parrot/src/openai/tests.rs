use super::*;

#[test]
fn openai_turn_preparation_has_no_subscription_policy() {
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
    let options = adapter.responses.prepare_turn(&mut request);
    assert_eq!(request.model, "grok-4.3");
    assert!(options.extra_headers.is_empty());
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
