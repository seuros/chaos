use super::*;

#[test]
fn experimental_realtime_start_instructions_load_from_config_toml() -> std::io::Result<()> {
    let cfg: ConfigToml = toml::from_str(
        r#"
experimental_realtime_start_instructions = "start instructions from config"
"#,
    )
    .expect("TOML deserialization should succeed");

    assert_eq!(
        cfg.experimental_realtime_start_instructions.as_deref(),
        Some("start instructions from config")
    );

    let chaos_home = TempDir::new()?;
    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.experimental_realtime_start_instructions.as_deref(),
        Some("start instructions from config")
    );
    Ok(())
}

#[test]
fn experimental_realtime_ws_base_url_loads_from_config_toml() -> std::io::Result<()> {
    let cfg: ConfigToml = toml::from_str(
        r#"
experimental_realtime_ws_base_url = "http://127.0.0.1:8011"
"#,
    )
    .expect("TOML deserialization should succeed");

    assert_eq!(
        cfg.experimental_realtime_ws_base_url.as_deref(),
        Some("http://127.0.0.1:8011")
    );

    let chaos_home = TempDir::new()?;
    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.experimental_realtime_ws_base_url.as_deref(),
        Some("http://127.0.0.1:8011")
    );
    Ok(())
}

#[test]
fn experimental_realtime_ws_backend_prompt_loads_from_config_toml() -> std::io::Result<()> {
    let cfg: ConfigToml = toml::from_str(
        r#"
experimental_realtime_ws_backend_prompt = "prompt from config"
"#,
    )
    .expect("TOML deserialization should succeed");

    assert_eq!(
        cfg.experimental_realtime_ws_backend_prompt.as_deref(),
        Some("prompt from config")
    );

    let chaos_home = TempDir::new()?;
    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.experimental_realtime_ws_backend_prompt.as_deref(),
        Some("prompt from config")
    );
    Ok(())
}

#[test]
fn experimental_realtime_ws_startup_context_loads_from_config_toml() -> std::io::Result<()> {
    let cfg: ConfigToml = toml::from_str(
        r#"
experimental_realtime_ws_startup_context = "startup context from config"
"#,
    )
    .expect("TOML deserialization should succeed");

    assert_eq!(
        cfg.experimental_realtime_ws_startup_context.as_deref(),
        Some("startup context from config")
    );

    let chaos_home = TempDir::new()?;
    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.experimental_realtime_ws_startup_context.as_deref(),
        Some("startup context from config")
    );
    Ok(())
}

#[test]
fn experimental_realtime_ws_model_loads_from_config_toml() -> std::io::Result<()> {
    let cfg: ConfigToml = toml::from_str(
        r#"
experimental_realtime_ws_model = "realtime-test-model"
"#,
    )
    .expect("TOML deserialization should succeed");

    assert_eq!(
        cfg.experimental_realtime_ws_model.as_deref(),
        Some("realtime-test-model")
    );

    let chaos_home = TempDir::new()?;
    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.experimental_realtime_ws_model.as_deref(),
        Some("realtime-test-model")
    );
    Ok(())
}

#[test]
fn realtime_loads_from_config_toml() -> std::io::Result<()> {
    let cfg: ConfigToml = toml::from_str(
        r#"
[realtime]
version = "v2"
type = "transcription"
"#,
    )
    .expect("TOML deserialization should succeed");

    assert_eq!(
        cfg.realtime,
        Some(RealtimeToml {
            version: Some(RealtimeWsVersion::V2),
            session_type: Some(RealtimeWsMode::Transcription),
        })
    );

    let chaos_home = TempDir::new()?;
    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.realtime,
        RealtimeConfig {
            version: RealtimeWsVersion::V2,
            session_type: RealtimeWsMode::Transcription,
        }
    );
    Ok(())
}

#[test]
fn realtime_audio_loads_from_config_toml() -> std::io::Result<()> {
    let cfg: ConfigToml = toml::from_str(
        r#"
[audio]
microphone = "USB Mic"
speaker = "Desk Speakers"
"#,
    )
    .expect("TOML deserialization should succeed");

    let realtime_audio = cfg
        .audio
        .as_ref()
        .expect("realtime audio config should be present");
    assert_eq!(realtime_audio.microphone.as_deref(), Some("USB Mic"));
    assert_eq!(realtime_audio.speaker.as_deref(), Some("Desk Speakers"));

    let chaos_home = TempDir::new()?;
    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(config.realtime_audio.microphone.as_deref(), Some("USB Mic"));
    assert_eq!(
        config.realtime_audio.speaker.as_deref(),
        Some("Desk Speakers")
    );
    Ok(())
}
