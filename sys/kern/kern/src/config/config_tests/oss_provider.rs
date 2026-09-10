use super::*;
use chaos_test_fixtures::TEST_MODEL;

#[tokio::test]
async fn test_set_default_oss_provider() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let chaos_home = temp_dir.path();
    crate::user_settings::install_persistence();

    // Test setting a provider on empty config
    set_default_oss_provider(chaos_home, "ollama")?;
    assert_eq!(
        crate::user_settings::snapshot(chaos_home).await?.settings["oss_provider"].as_str(),
        Some("ollama")
    );

    // Test updating existing config
    ConfigEditsBuilder::new(chaos_home)
        .set_model(Some(TEST_MODEL), None)
        .apply()
        .await?;
    set_default_oss_provider(chaos_home, "lmstudio")?;
    let settings = crate::user_settings::snapshot(chaos_home).await?.settings;
    assert_eq!(settings["oss_provider"].as_str(), Some("lmstudio"));
    assert_eq!(settings["model"].as_str(), Some(TEST_MODEL));

    // Test overwriting existing oss_provider
    set_default_oss_provider(chaos_home, "ollama")?;
    assert_eq!(
        crate::user_settings::snapshot(chaos_home).await?.settings["oss_provider"].as_str(),
        Some("ollama")
    );

    // Test that an empty provider is rejected
    let result = set_default_oss_provider(chaos_home, "");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);

    Ok(())
}
#[test]
fn test_resolve_oss_provider_explicit_override() {
    let config_toml = ConfigToml::default();
    let result = resolve_oss_provider(Some("custom-provider"), &config_toml, None);
    assert_eq!(result, Some("custom-provider".to_string()));
}

#[test]
fn test_resolve_oss_provider_from_profile() {
    let mut profiles = std::collections::HashMap::new();
    let profile = ConfigProfile {
        oss_provider: Some("profile-provider".to_string()),
        ..Default::default()
    };
    profiles.insert("test-profile".to_string(), profile);
    let config_toml = ConfigToml {
        profiles,
        ..Default::default()
    };

    let result = resolve_oss_provider(None, &config_toml, Some("test-profile".to_string()));
    assert_eq!(result, Some("profile-provider".to_string()));
}

#[test]
fn test_resolve_oss_provider_from_global_config() {
    let config_toml = ConfigToml {
        oss_provider: Some("global-provider".to_string()),
        ..Default::default()
    };

    let result = resolve_oss_provider(None, &config_toml, None);
    assert_eq!(result, Some("global-provider".to_string()));
}

#[test]
fn test_resolve_oss_provider_profile_fallback_to_global() {
    let mut profiles = std::collections::HashMap::new();
    let profile = ConfigProfile::default(); // No oss_provider set
    profiles.insert("test-profile".to_string(), profile);
    let config_toml = ConfigToml {
        oss_provider: Some("global-provider".to_string()),
        profiles,
        ..Default::default()
    };

    let result = resolve_oss_provider(None, &config_toml, Some("test-profile".to_string()));
    assert_eq!(result, Some("global-provider".to_string()));
}

#[test]
fn test_resolve_oss_provider_none_when_not_configured() {
    let config_toml = ConfigToml::default();
    let result = resolve_oss_provider(None, &config_toml, None);
    assert_eq!(result, None);
}

#[test]
fn test_resolve_oss_provider_explicit_overrides_all() {
    let mut profiles = std::collections::HashMap::new();
    let profile = ConfigProfile {
        oss_provider: Some("profile-provider".to_string()),
        ..Default::default()
    };
    profiles.insert("test-profile".to_string(), profile);
    let config_toml = ConfigToml {
        oss_provider: Some("global-provider".to_string()),
        profiles,
        ..Default::default()
    };

    let result = resolve_oss_provider(
        Some("explicit-provider"),
        &config_toml,
        Some("test-profile".to_string()),
    );
    assert_eq!(result, Some("explicit-provider".to_string()));
}
