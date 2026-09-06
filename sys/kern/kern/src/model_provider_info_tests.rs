use super::*;
use pretty_assertions::assert_eq;

fn provider_fixture(name: &str, base_url: &str) -> ModelProviderInfo {
    ModelProviderInfo {
        name: name.into(),
        model_family: Default::default(),
        model_family_overrides: Default::default(),
        base_url: Some(base_url.into()),
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
    }
}

#[test]
fn built_in_family_registry_is_exact_and_preserves_catalog_authority() {
    let endpoint = "https://hyper.charm.land/v1";
    let unknown = "unknown";
    for (binding, url, model, catalog, expected) in [
        ("charm", endpoint, "deepseek-v4-pro", "unknown", "deepseek"),
        ("charm", endpoint, "deepseek-v4-pro", "catalog", "catalog"),
        ("other", endpoint, "deepseek-v4-pro", "unknown", "unknown"),
        (
            "charm",
            "https://other.test/v1",
            "deepseek-v4-pro",
            unknown,
            unknown,
        ),
        ("charm", endpoint, "ns/deepseek-v4-pro", unknown, unknown),
        ("charm", endpoint, "deepseek-v4-pro-0813", unknown, unknown),
    ] {
        let provider = provider_fixture("Gateway", url);
        assert_eq!(
            provider.family_for_model(binding, model, &ModelFamily::new(catalog)),
            ModelFamily::new(expected),
            "{binding} / {url} / {model}",
        );
    }
    let mut provider = provider_fixture("Gateway", endpoint);
    provider
        .model_family_overrides
        .insert("deepseek-v4-pro".into(), ModelFamily::new("configured"));
    assert_eq!(
        provider.family_for_model("charm", "deepseek-v4-pro", &ModelFamily::default()),
        ModelFamily::new("deepseek"),
    );
}

#[test]
fn test_deserialize_ollama_model_provider_toml() {
    let azure_provider_toml = r#"
name = "Ollama"
base_url = "http://localhost:11434/v1"
        "#;
    let expected_provider = provider_fixture("Ollama", "http://localhost:11434/v1");

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
        env_key: Some("AZURE_OPENAI_API_KEY".into()),
        query_params: Some(HashMap::from([(
            "api-version".to_string(),
            "2025-04-01-preview".to_string(),
        )])),
        ..provider_fixture("Azure", "https://xxxxx.openai.azure.com/openai")
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
        env_key: Some("API_KEY".into()),
        http_headers: Some(HashMap::from([(
            "X-Example-Header".to_string(),
            "example-value".to_string(),
        )])),
        env_http_headers: Some(HashMap::from([(
            "X-Example-Env-Header".to_string(),
            "EXAMPLE_ENV_VAR".to_string(),
        )])),
        ..provider_fixture("Example", "https://example.com")
    };

    let provider: ModelProviderInfo = toml::from_str(azure_provider_toml).unwrap();
    assert_eq!(expected_provider, provider);
}

#[test]
fn model_family_overrides_serde_validates_exact_keys_and_round_trips() {
    let provider: ModelProviderInfo = toml::from_str(
        "name = 'Provider'\n[model_family_overrides]\n\
         'model-alpha' = '  Family.A  '\n'ns/model-alpha' = 'family-b'",
    )
    .unwrap();
    assert_eq!(
        provider.model_family_overrides,
        HashMap::from([
            ("model-alpha".into(), ModelFamily::new("family.a")),
            ("ns/model-alpha".into(), ModelFamily::new("family-b")),
        ])
    );
    let rendered = toml::to_string(&provider).unwrap();
    assert_eq!(provider, toml::from_str(&rendered).unwrap());
}

#[test]
fn model_family_overrides_serde_rejects_invalid_keys_and_values() {
    for entry in [
        "'' = 'family-a'",
        "' model' = 'family-a'",
        "'model ' = 'family-a'",
        "'model*' = 'family-a'",
        "'model?' = 'family-a'",
        "'model' = ''",
        "'model' = 'not a family'",
        "'model' = 'unknown'",
    ] {
        let input = format!("name = 'Provider'\n[model_family_overrides]\n{entry}");
        let err = toml::from_str::<ModelProviderInfo>(&input).expect_err(entry);
        assert!(
            err.to_string().contains("model_family_overrides"),
            "{entry}: unexpected error: {err}"
        );
    }
}

#[test]
fn model_family_overrides_legacy_default_round_trips_without_emitting_empty_table() {
    let provider: ModelProviderInfo = toml::from_str("name = 'Provider'").unwrap();
    assert!(provider.model_family_overrides.is_empty());
    let rendered = toml::to_string(&provider).unwrap();
    assert!(!rendered.contains("model_family_overrides"));
    assert_eq!(provider, toml::from_str(&rendered).unwrap());
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
