use super::*;
use chaos_test_fixtures::TEST_MODEL;

#[test]
fn config_toml_deserializes_mcp_oauth_callback_port() {
    let toml = r#"mcp_oauth_callback_port = 4321"#;
    let cfg: ConfigToml =
        toml::from_str(toml).expect("TOML deserialization should succeed for callback port");
    assert_eq!(cfg.mcp_oauth_callback_port, Some(4321));
}

#[test]
fn config_toml_deserializes_mcp_oauth_callback_url() {
    let toml = r#"mcp_oauth_callback_url = "https://example.com/callback""#;
    let cfg: ConfigToml =
        toml::from_str(toml).expect("TOML deserialization should succeed for callback URL");
    assert_eq!(
        cfg.mcp_oauth_callback_url.as_deref(),
        Some("https://example.com/callback")
    );
}

#[test]
fn config_loads_mcp_oauth_callback_port_from_toml() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let toml = format!(
        r#"
model = "{TEST_MODEL}"
mcp_oauth_callback_port = 5678
"#
    );
    let cfg: ConfigToml =
        toml::from_str(&toml).expect("TOML deserialization should succeed for callback port");

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(config.mcp_oauth_callback_port, Some(5678));
    Ok(())
}

#[test]
fn config_loads_allow_login_shell_from_toml() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cfg: ConfigToml = toml::from_str(&format!(
        r#"
model = "{TEST_MODEL}"
allow_login_shell = false
"#
    ))
    .expect("TOML deserialization should succeed for allow_login_shell");

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert!(!config.permissions.allow_login_shell);
    Ok(())
}

#[tokio::test]
async fn shell_setting_round_trips_through_database() -> anyhow::Result<()> {
    let chaos_home = TempDir::new()?;
    crate::test_support::bootstrap_sqlite(chaos_home.path())?;
    crate::user_settings::install_persistence();
    crate::test_support::seed_user_settings_toml(chaos_home.path(), r#"shell = "zsh""#).await?;

    let cfg: ConfigToml = crate::user_settings::effective_settings(chaos_home.path())
        .await?
        .try_into()?;
    assert_eq!(cfg.shell.as_deref(), Some("zsh"));

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;
    assert_eq!(config.shell.as_deref(), Some("zsh"));
    Ok(())
}

#[test]
fn config_loads_mcp_oauth_callback_url_from_toml() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let toml = format!(
        r#"
model = "{TEST_MODEL}"
mcp_oauth_callback_url = "https://example.com/callback"
"#
    );
    let cfg: ConfigToml =
        toml::from_str(&toml).expect("TOML deserialization should succeed for callback URL");

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.mcp_oauth_callback_url.as_deref(),
        Some("https://example.com/callback")
    );
    Ok(())
}
