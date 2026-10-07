use super::*;

#[test]
fn identifies_only_the_official_grok_subscription_proxy() {
    assert!(is_grok_subscription_proxy(
        "https://cli-chat-proxy.grok.com/v1"
    ));
    assert!(!is_grok_subscription_proxy("https://api.x.ai/v1"));
    assert!(!is_grok_subscription_proxy(
        "https://cli-chat-proxy.grok.com.attacker.example/v1"
    ));
}

#[test]
fn grok_subscription_requests_route_by_model_header() {
    let adapter = XaiAdapter::from_base_url_and_api_key(
        "https://cli-chat-proxy.grok.com/v1".to_string(),
        "fixture-token".to_string(),
        Some("grok-4.3".to_string()),
    );
    let mut request = empty_request();
    request.model = "grok-4.3".to_string();
    let options = adapter.prepare_turn(&mut request);

    assert_eq!(
        options
            .extra_headers
            .get(GROK_MODEL_OVERRIDE_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("grok-4.3")
    );
    assert_eq!(
        options
            .extra_headers
            .get(GROK_CLIENT_VERSION_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert!(!options.extra_headers.contains_key(XAI_TOKEN_AUTH_HEADER));
}

#[test]
fn grok_subscription_proxy_models_are_known_to_see() {
    let proxy = "https://cli-chat-proxy.grok.com/v1";
    // The subscription proxy accepts images despite its text-only listing.
    for id in [
        "grok-4.5",
        "grok-4.6",
        "grok-4.7",
        "grok-4.7-build-fast",
        "grok-4",
        "grok-4-fast",
        "GROK-4.7",
    ] {
        assert!(resolve_supports_images(proxy, id, None), "{id} silent");
        assert!(
            resolve_supports_images(proxy, id, Some(false)),
            "{id} said no"
        );
    }
    // Coding models stay text-only unless the proxy says otherwise.
    assert!(!resolve_supports_images(proxy, "grok-code-fast-1", None));
    assert!(!resolve_supports_images(proxy, "grok-4-code", None));
    for id in ["grok-3", "grok-40", "grok-4unknown", "other-model"] {
        assert!(!resolve_supports_images(proxy, id, None), "{id}");
    }
    assert!(resolve_supports_images(
        proxy,
        "grok-code-fast-1",
        Some(true)
    ));

    // Off the proxy, the catalog is taken at its word.
    for base in [
        "https://api.x.ai/v1",
        "https://cli-chat-proxy.grok.com.attacker.example/v1",
    ] {
        assert!(!resolve_supports_images(base, "grok-4.6", None), "{base}");
        assert!(
            !resolve_supports_images(base, "grok-4.6", Some(false)),
            "{base}"
        );
        assert!(
            resolve_supports_images(base, "grok-4.6", Some(true)),
            "{base}"
        );
    }
}

#[test]
fn official_endpoint_recognition_uses_exact_parsed_hosts() {
    for base in [
        "https://api.x.ai/v1",
        "https://API.X.AI:443/v1",
        "https://cli-chat-proxy.grok.com/v1",
    ] {
        assert!(is_xai_endpoint(base), "{base}");
    }
    for base in [
        "https://api.x.ai.attacker.example/v1",
        "https://cli-chat-proxy.grok.com.attacker.example/v1",
        "https://api.x.ai@attacker.example/v1",
        "https://attacker.example/api.x.ai/v1",
        "https://attacker.example/v1?api=https://api.x.ai",
        "https://x.ai/v1",
        "https://custom.example/v1",
        "api.x.ai/v1",
        "not a URL",
    ] {
        assert!(!is_xai_endpoint(base), "{base}");
        assert!(!is_grok_subscription_proxy(base), "{base}");
    }
}

#[test]
fn native_tools_belong_only_to_the_exact_xai_api_host() {
    assert_eq!(
        native_tools_for_base_url("https://api.x.ai/v1"),
        ["web_search", "x_search"]
    );
    for base in [
        "https://cli-chat-proxy.grok.com/v1",
        "https://custom.example/v1",
        "https://api.x.ai.attacker.example/v1",
        "https://api.x.ai@attacker.example/v1",
        "https://attacker.example/api.x.ai/v1",
        "https://attacker.example/?host=x.ai",
    ] {
        assert!(native_tools_for_base_url(base).is_empty(), "{base}");
    }
}

#[test]
fn subscription_headers_override_turn_headers_after_default_model_selection() {
    use crate::requests::responses::Compression;

    let mut defaults = ResponsesOptions {
        compression: Compression::Zstd,
        ..ResponsesOptions::default()
    };
    defaults
        .extra_headers
        .insert("x-default", HeaderValue::from_static("removed"));
    let adapter = XaiAdapter::from_base_url_and_api_key(
        "https://cli-chat-proxy.grok.com/v1".to_string(),
        "fixture-token".to_string(),
        Some("grok-4.6".to_string()),
    )
    .with_options(defaults);
    let mut request = empty_request();
    request.extensions.insert(
        "request_headers".to_string(),
        serde_json::json!({
            "x-turn": "preserved",
            "x-grok-client-version": "wrong",
            "x-grok-model-override": "wrong",
            "x-xai-token-auth": "wrong",
        }),
    );
    request
        .extensions
        .insert("compression".to_string(), serde_json::json!("none"));
    let options = adapter.prepare_turn(&mut request);
    assert_eq!(request.model, "grok-4.6");
    assert_eq!(
        options.extra_headers[GROK_MODEL_OVERRIDE_HEADER],
        "grok-4.6"
    );
    assert_eq!(
        options.extra_headers[GROK_CLIENT_VERSION_HEADER],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(options.extra_headers[XAI_TOKEN_AUTH_HEADER], "wrong");
    assert_eq!(options.extra_headers["x-turn"], "preserved");
    assert!(!options.extra_headers.contains_key("x-default"));
    std::assert_matches!(options.compression, Compression::None);
}

#[test]
fn subscription_headers_are_not_injected_for_api_custom_or_lookalike_endpoints() {
    for base in [
        "https://api.x.ai/v1",
        "https://custom.example/v1",
        "https://cli-chat-proxy.grok.com.attacker.example/v1",
        "https://cli-chat-proxy.grok.com@attacker.example/v1",
        "https://attacker.example/cli-chat-proxy.grok.com/v1",
    ] {
        let adapter = XaiAdapter::from_base_url_and_api_key(
            base.to_string(),
            "fixture-token".to_string(),
            Some("grok-4.6".to_string()),
        );
        let options = adapter.prepare_turn(&mut empty_request());
        assert!(options.extra_headers.is_empty(), "{base}");
        let mut discovery_headers = HeaderMap::new();
        insert_grok_subscription_auth_headers(base, &mut discovery_headers);
        assert!(discovery_headers.is_empty(), "{base}");
    }
}

#[test]
fn proxy_discovery_headers_authenticate_without_model_routing() {
    let mut headers = HeaderMap::new();
    insert_grok_subscription_auth_headers("https://cli-chat-proxy.grok.com/v1", &mut headers);
    assert_eq!(headers[XAI_TOKEN_AUTH_HEADER], XAI_TOKEN_AUTH_VALUE);
    assert_eq!(
        headers[GROK_CLIENT_VERSION_HEADER],
        env!("CARGO_PKG_VERSION")
    );
    assert!(!headers.contains_key(GROK_MODEL_OVERRIDE_HEADER));
}

#[test]
fn discovery_policy_applies_image_fallback_and_native_tools_independently() {
    let model = chaos_abi::AbiModelInfo {
        id: "grok-4.6".to_string(),
        display_name: "Grok".to_string(),
        model_family: Default::default(),
        description: None,
        max_input_tokens: None,
        max_output_tokens: None,
        supports_thinking: false,
        supports_images: false,
        supports_structured_output: false,
        supports_reasoning_effort: false,
        native_server_side_tools: vec![],
    };
    let mut proxy_models = vec![model.clone()];
    apply_model_policy("https://cli-chat-proxy.grok.com/v1", &mut proxy_models);
    assert!(proxy_models[0].supports_images);
    assert!(proxy_models[0].native_server_side_tools.is_empty());

    let mut api_models = vec![model.clone()];
    apply_model_policy("https://api.x.ai/v1", &mut api_models);
    assert!(!api_models[0].supports_images);
    assert_eq!(
        api_models[0].native_server_side_tools,
        ["web_search", "x_search"]
    );

    let mut custom_models = vec![model];
    apply_model_policy("https://custom.example/v1", &mut custom_models);
    assert!(!custom_models[0].supports_images);
    assert!(custom_models[0].native_server_side_tools.is_empty());
}

fn empty_request() -> TurnRequest {
    TurnRequest {
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
    }
}
