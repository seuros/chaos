use super::*;
use crate::config::requirements::resolve_egress_url;
use crate::config_loader::ConfigLayerStack;
use crate::user_settings::ResolvedBootstrap;
use std::path::Path;

fn load_with_egress(
    cfg: ConfigToml,
    home: &Path,
    egress_url: Option<String>,
) -> std::io::Result<Config> {
    Config::load_config_with_layer_stack(
        cfg,
        ConfigOverrides::default(),
        home.to_path_buf(),
        ConfigLayerStack::default(),
        ResolvedBootstrap {
            egress_url,
            ..ResolvedBootstrap::sqlite_in(home)
        },
    )
}

#[test]
fn global_egress_is_inherited_by_every_provider() {
    let home = tempdir().unwrap();
    let parsed: ConfigToml = toml::from_str(
        r#"
model_provider = "custom"

[model_providers.custom]
name = "Custom"
base_url = "https://custom.example/v1"
wire_api = "chat_completions"
"#,
    )
    .unwrap();
    let bootstrap_endpoint = "http://gateway.example:8847/egress/chaos";
    let endpoint = "http://localhost:8847/egress/chaos";
    for configured in [None, Some(bootstrap_endpoint.to_string())] {
        assert_eq!(
            resolve_egress_url(configured, Some(endpoint.into())).unwrap(),
            Some(endpoint.to_string()),
            "CHAOS_EGRESS_URL overrides the bootstrap egress"
        );
    }
    let config = load_with_egress(parsed, home.path(), Some(endpoint.to_string())).unwrap();
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
    // Egress is bootstrap-only: neither a settings key nor a per-provider option.
    let serialized = serde_json::to_value(&config.model_provider).unwrap();
    assert!(serialized.get("egress").is_none());
    let schema = serde_json::to_value(super::super::schema::config_schema()).unwrap();
    assert!(schema["properties"].get("egress_url").is_none());
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
        let egress_url = resolve_egress_url(
            Some("http://localhost:8847/egress/chaos".to_string()),
            Some(value.into()),
        )
        .unwrap();
        let result = load_with_egress(ConfigToml::default(), home.path(), egress_url);
        assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidInput);
    }
}
