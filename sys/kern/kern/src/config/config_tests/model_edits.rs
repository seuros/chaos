use super::*;

#[tokio::test]
async fn set_model_updates_defaults() -> anyhow::Result<()> {
    let chaos_home = TempDir::new()?;
    crate::user_settings::install_persistence();

    ConfigEditsBuilder::new(chaos_home.path())
        .set_model(Some("serpent"), Some(ReasoningEffort::High))
        .apply()
        .await?;

    let parsed: ConfigToml = crate::user_settings::snapshot(chaos_home.path())
        .await?
        .settings
        .try_into()?;

    assert_eq!(parsed.model.as_deref(), Some("serpent"));
    assert_eq!(parsed.model_reasoning_effort, Some(ReasoningEffort::High));

    Ok(())
}

#[tokio::test]
async fn set_model_overwrites_existing_model() -> anyhow::Result<()> {
    crate::user_settings::install_persistence();
    let chaos_home = TempDir::new()?;
    let config_path = chaos_home.path().join(CONFIG_TOML_FILE);

    tokio::fs::write(
        &config_path,
        r#"
model = "serpent"
model_reasoning_effort = "medium"

[profiles.dev]
model = "gordon"
"#,
    )
    .await?;

    crate::user_settings::migrate(chaos_home.path(), false).await?;
    ConfigEditsBuilder::new(chaos_home.path())
        .set_model(Some("o4-mini"), Some(ReasoningEffort::High))
        .apply()
        .await?;

    let parsed: ConfigToml = crate::user_settings::snapshot(chaos_home.path())
        .await?
        .settings
        .try_into()?;

    assert_eq!(parsed.model.as_deref(), Some("o4-mini"));
    assert_eq!(parsed.model_reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(
        parsed
            .profiles
            .get("dev")
            .and_then(|profile| profile.model.as_deref()),
        Some("gordon"),
    );

    Ok(())
}

#[tokio::test]
async fn set_model_updates_profile() -> anyhow::Result<()> {
    let chaos_home = TempDir::new()?;
    crate::user_settings::install_persistence();

    ConfigEditsBuilder::new(chaos_home.path())
        .with_profile(Some("dev"))
        .set_model(Some("serpent"), Some(ReasoningEffort::Medium))
        .apply()
        .await?;

    let parsed: ConfigToml = crate::user_settings::snapshot(chaos_home.path())
        .await?
        .settings
        .try_into()?;
    let profile = parsed
        .profiles
        .get("dev")
        .expect("profile should be created");

    assert_eq!(profile.model.as_deref(), Some("serpent"));
    assert_eq!(
        profile.model_reasoning_effort,
        Some(ReasoningEffort::Medium)
    );

    Ok(())
}

#[tokio::test]
async fn set_model_updates_existing_profile() -> anyhow::Result<()> {
    crate::user_settings::install_persistence();
    let chaos_home = TempDir::new()?;
    let config_path = chaos_home.path().join(CONFIG_TOML_FILE);

    tokio::fs::write(
        &config_path,
        r#"
[profiles.dev]
model = "gordon"
model_reasoning_effort = "medium"

[profiles.prod]
model = "sherlock"
"#,
    )
    .await?;

    crate::user_settings::migrate(chaos_home.path(), false).await?;
    ConfigEditsBuilder::new(chaos_home.path())
        .with_profile(Some("dev"))
        .set_model(Some("o4-high"), Some(ReasoningEffort::Medium))
        .apply()
        .await?;

    let parsed: ConfigToml = crate::user_settings::snapshot(chaos_home.path())
        .await?
        .settings
        .try_into()?;

    let dev_profile = parsed
        .profiles
        .get("dev")
        .expect("dev profile should survive updates");
    assert_eq!(dev_profile.model.as_deref(), Some("o4-high"));
    assert_eq!(
        dev_profile.model_reasoning_effort,
        Some(ReasoningEffort::Medium)
    );

    assert_eq!(
        parsed
            .profiles
            .get("prod")
            .and_then(|profile| profile.model.as_deref()),
        Some("sherlock"),
    );

    Ok(())
}
