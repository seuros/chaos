use super::*;
use pretty_assertions::assert_eq;

#[test]
fn test_deserialize_ollama_model_provider_toml() {
    let azure_provider_toml = r#"
name = "Ollama"
base_url = "http://localhost:11434/v1"
        "#;
    let expected_provider = ModelProviderInfo {
        name: "Ollama".into(),
        model_family: Default::default(),
        base_url: Some("http://localhost:11434/v1".into()),
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        wire_api: WireApi::Auto,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        requires_openai_auth: false,
        auth: None,
        supports_websockets: false,
        native_server_side_tools: vec![],
    };

    let provider: ModelProviderInfo = toml::from_str(azure_provider_toml).unwrap();
    assert_eq!(expected_provider, provider);
}

#[test]
fn test_deserialize_azure_model_provider_toml() {
    let azure_provider_toml = r#"
name = "Azure"
base_url = "https://xxxxx.openai.azure.com/openai"
env_key = "AZURE_OPENAI_API_KEY"
query_params = { api-version = "2025-04-01-preview" }
        "#;
    let expected_provider = ModelProviderInfo {
        name: "Azure".into(),
        model_family: Default::default(),
        base_url: Some("https://xxxxx.openai.azure.com/openai".into()),
        env_key: Some("AZURE_OPENAI_API_KEY".into()),
        env_key_instructions: None,
        experimental_bearer_token: None,
        wire_api: WireApi::Auto,
        query_params: Some(HashMap::from([(
            "api-version".to_string(),
            "2025-04-01-preview".to_string(),
        )])),
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        requires_openai_auth: false,
        auth: None,
        supports_websockets: false,
        native_server_side_tools: vec![],
    };

    let provider: ModelProviderInfo = toml::from_str(azure_provider_toml).unwrap();
    assert_eq!(expected_provider, provider);
}

#[test]
fn test_deserialize_example_model_provider_toml() {
    let azure_provider_toml = r#"
name = "Example"
base_url = "https://example.com"
env_key = "API_KEY"
http_headers = { "X-Example-Header" = "example-value" }
env_http_headers = { "X-Example-Env-Header" = "EXAMPLE_ENV_VAR" }
        "#;
    let expected_provider = ModelProviderInfo {
        name: "Example".into(),
        model_family: Default::default(),
        base_url: Some("https://example.com".into()),
        env_key: Some("API_KEY".into()),
        env_key_instructions: None,
        experimental_bearer_token: None,
        wire_api: WireApi::Auto,
        query_params: None,
        http_headers: Some(HashMap::from([(
            "X-Example-Header".to_string(),
            "example-value".to_string(),
        )])),
        env_http_headers: Some(HashMap::from([(
            "X-Example-Env-Header".to_string(),
            "EXAMPLE_ENV_VAR".to_string(),
        )])),
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        requires_openai_auth: false,
        auth: None,
        supports_websockets: false,
        native_server_side_tools: vec![],
    };

    let provider: ModelProviderInfo = toml::from_str(azure_provider_toml).unwrap();
    assert_eq!(expected_provider, provider);
}

#[test]
fn xai_subscription_auth_adds_cli_token_header_only_for_oauth() {
    let provider = built_in_model_providers()
        .remove("xai")
        .expect("xAI provider should be built in");

    let oauth = provider
        .to_api_provider(Some(AuthMode::Xai))
        .expect("xAI OAuth provider should build");
    assert_eq!(
        oauth.base_url, "https://cli-chat-proxy.grok.com/v1",
        "subscription auth must use xAI's CLI proxy rather than the API-key endpoint"
    );
    assert_eq!(
        oauth
            .headers
            .get(XAI_TOKEN_AUTH_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(XAI_TOKEN_AUTH_VALUE)
    );

    let api_key = provider
        .to_api_provider(Some(AuthMode::ApiKey))
        .expect("xAI API-key provider should build");
    assert_eq!(api_key.base_url, "https://api.x.ai/v1");
    assert!(api_key.headers.get(XAI_TOKEN_AUTH_HEADER).is_none());
}
