use super::*;
use crate::config::requirements::resolve_egress_url;

#[test]
fn global_egress_is_inherited_by_every_provider() {
    let home = tempdir().unwrap();
    let mut parsed: ConfigToml = toml::from_str(
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
    let endpoint = "http://localhost:8847/egress/chaos";
    for configured in [None, parsed.egress_url.clone()] {
        assert_eq!(
            resolve_egress_url(configured, Some(endpoint.into())).unwrap(),
            Some(endpoint.to_string())
        );
    }
    parsed.egress_url = resolve_egress_url(parsed.egress_url, Some(endpoint.into())).unwrap();
    let config = Config::load_from_base_config_with_overrides(
        parsed,
        ConfigOverrides::default(),
        home.path().to_path_buf(),
    )
    .unwrap();
    assert_eq!(config.egress_url.as_deref(), Some(endpoint));
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
    assert_eq!(resolve_egress_url(None, None).unwrap(), None);
    let endpoint = Some("http://localhost:8847/egress/chaos".to_string());
    assert_eq!(
        resolve_egress_url(endpoint.clone(), None).unwrap(),
        endpoint
    );
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
    for value in ["not a gateway URL", ""] {
        let result = Config::load_from_base_config_with_overrides(
            ConfigToml {
                egress_url: resolve_egress_url(
                    Some("http://localhost:8847/egress/chaos".to_string()),
                    Some(value.into()),
                )
                .unwrap(),
                ..Default::default()
            },
            ConfigOverrides::default(),
            home.path().to_path_buf(),
        );
        assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidInput);
    }
}
