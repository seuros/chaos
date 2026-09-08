use super::*;

#[test]
fn global_egress_is_inherited_by_every_provider() {
    let home = tempdir().unwrap();
    let parsed: ConfigToml = toml::from_str(
        r#"
egress_url = "http://192.168.3.21:8847/egress/chaos"
model_provider = "custom"

[model_providers.custom]
name = "Custom"
base_url = "https://custom.example/v1"
wire_api = "chat_completions"
"#,
    )
    .unwrap();
    let config = Config::load_from_base_config_with_overrides(
        parsed,
        ConfigOverrides::default(),
        home.path().to_path_buf(),
    )
    .unwrap();
    let endpoint = config.egress_url.as_deref().unwrap();
    assert_eq!(
        config.model_provider.egress.as_ref().unwrap().endpoint(),
        endpoint
    );
    for (id, provider) in &config.model_providers {
        assert_eq!(
            provider.egress.as_ref().unwrap().endpoint(),
            endpoint,
            "{id} must inherit global egress"
        );
        let api_provider = provider.to_api_provider(None).unwrap();
        assert_eq!(api_provider.egress, provider.egress);
        assert!(!api_provider.base_url.contains("/egress/"));
    }
    // Egress must not become a per-provider config/schema option.
    let serialized = serde_json::to_value(&config.model_provider).unwrap();
    assert!(serialized.get("egress").is_none());
    let schema = serde_json::to_value(super::super::schema::config_schema()).unwrap();
    assert!(schema["properties"].get("egress_url").is_some());
    assert!(
        schema["definitions"]["ModelProviderInfo"]["properties"]
            .get("egress")
            .is_none()
    );
}

#[test]
fn no_egress_configuration_keeps_all_providers_direct() {
    let home = tempdir().unwrap();
    let config = Config::load_from_base_config_with_overrides(
        ConfigToml::default(),
        ConfigOverrides::default(),
        home.path().to_path_buf(),
    )
    .unwrap();
    assert!(config.egress_url.is_none());
    assert!(
        config
            .model_providers
            .values()
            .all(|provider| provider.egress.is_none())
    );
}

#[test]
fn invalid_global_egress_fails_config_load() {
    let home = tempdir().unwrap();
    let result = Config::load_from_base_config_with_overrides(
        ConfigToml {
            egress_url: Some("not a gateway URL".to_string()),
            ..Default::default()
        },
        ConfigOverrides::default(),
        home.path().to_path_buf(),
    );
    assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidInput);
}
