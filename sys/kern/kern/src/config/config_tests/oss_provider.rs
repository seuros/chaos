use super::*;
use chaos_test_fixtures::TEST_MODEL;

#[tokio::test]
async fn test_set_default_oss_provider() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    crate::test_support::bootstrap_sqlite(temp_dir.path())?;
    let chaos_home = temp_dir.path();
    crate::user_settings::install_persistence();

    // Test setting a provider on empty config
    set_default_oss_provider(chaos_home, "ollama")?;
    assert_eq!(
        crate::user_settings::installation_snapshot(chaos_home)
            .await?
            .settings["oss_provider"]
            .as_str(),
        Some("ollama")
    );

    // Test updating existing config
    ConfigEditsBuilder::new(chaos_home)
        .set_model(Some(TEST_MODEL), None)
        .apply()
        .await?;
    set_default_oss_provider(chaos_home, "lmstudio")?;
    let settings = crate::user_settings::installation_snapshot(chaos_home)
        .await?
        .settings;
    assert_eq!(settings["oss_provider"].as_str(), Some("lmstudio"));
    assert_eq!(settings["model"].as_str(), Some(TEST_MODEL));

    // Test overwriting existing oss_provider
    set_default_oss_provider(chaos_home, "ollama")?;
    assert_eq!(
        crate::user_settings::installation_snapshot(chaos_home)
            .await?
            .settings["oss_provider"]
            .as_str(),
        Some("ollama")
    );

    // Test that an empty provider is rejected
    let result = set_default_oss_provider(chaos_home, "");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);

    Ok(())
}
#[test]
fn resolve_oss_provider_obeys_explicit_profile_global_precedence() {
    for (explicit, profile, global, selected_profile, expected) in [
        (
            Some("custom-provider"),
            None,
            None,
            false,
            Some("custom-provider"),
        ),
        (
            None,
            Some("profile-provider"),
            None,
            true,
            Some("profile-provider"),
        ),
        (
            None,
            None,
            Some("global-provider"),
            false,
            Some("global-provider"),
        ),
        (
            None,
            None,
            Some("global-provider"),
            true,
            Some("global-provider"),
        ),
        (None, None, None, false, None),
        (
            Some("explicit-provider"),
            Some("profile-provider"),
            Some("global-provider"),
            true,
            Some("explicit-provider"),
        ),
    ] {
        let mut config_toml = ConfigToml {
            oss_provider: global.map(str::to_string),
            ..Default::default()
        };
        if selected_profile {
            config_toml.profiles.insert(
                "test-profile".to_string(),
                ConfigProfile {
                    oss_provider: profile.map(str::to_string),
                    ..Default::default()
                },
            );
        }
        let result = resolve_oss_provider(
            explicit,
            &config_toml,
            selected_profile.then(|| "test-profile".to_string()),
        );
        assert_eq!(
            result.as_deref(),
            expected,
            "explicit={explicit:?}, profile={profile:?}, global={global:?}"
        );
    }
}
