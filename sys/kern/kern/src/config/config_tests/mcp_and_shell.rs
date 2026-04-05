use super::*;

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
    let toml = r#"
model = "gpt-5.1"
mcp_oauth_callback_port = 5678
"#;
    let cfg: ConfigToml =
        toml::from_str(toml).expect("TOML deserialization should succeed for callback port");

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
    let cfg: ConfigToml = toml::from_str(
        r#"
model = "gpt-5.1"
allow_login_shell = false
"#,
    )
    .expect("TOML deserialization should succeed for allow_login_shell");

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert!(!config.permissions.allow_login_shell);
    Ok(())
}

#[test]
fn config_loads_mcp_oauth_callback_url_from_toml() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let toml = r#"
model = "gpt-5.1"
mcp_oauth_callback_url = "https://example.com/callback"
"#;
    let cfg: ConfigToml =
        toml::from_str(toml).expect("TOML deserialization should succeed for callback URL");

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
