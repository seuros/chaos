use super::*;

#[tokio::test]
async fn set_model_updates_defaults() -> anyhow::Result<()> {
    let chaos_home = TempDir::new()?;
    crate::test_support::bootstrap_sqlite(chaos_home.path())?;
    crate::user_settings::install_persistence();

    ConfigEditsBuilder::new(chaos_home.path())
        .set_model(Some("serpent"), Some(ReasoningEffort::High))
        .apply()
        .await?;

    let parsed: ConfigToml = crate::user_settings::effective_settings(chaos_home.path())
        .await?
        .try_into()?;

    assert_eq!(parsed.model.as_deref(), Some("serpent"));
    assert_eq!(parsed.model_reasoning_effort, Some(ReasoningEffort::High));

    Ok(())
}

#[tokio::test]
async fn set_model_overwrites_existing_model() -> anyhow::Result<()> {
    crate::user_settings::install_persistence();
    let chaos_home = TempDir::new()?;
    crate::test_support::seed_user_settings_toml(
        chaos_home.path(),
        r#"
model = "serpent"
model_reasoning_effort = "medium"

[profiles.dev]
model = "gordon"
"#,
    )
    .await?;
    ConfigEditsBuilder::new(chaos_home.path())
        .set_model(Some("o4-mini"), Some(ReasoningEffort::High))
        .apply()
        .await?;

    let parsed: ConfigToml = crate::user_settings::effective_settings(chaos_home.path())
        .await?
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
    crate::test_support::bootstrap_sqlite(chaos_home.path())?;
    crate::user_settings::install_persistence();

    ConfigEditsBuilder::new(chaos_home.path())
        .with_profile(Some("dev"))
        .set_model(Some("serpent"), Some(ReasoningEffort::Medium))
        .apply()
        .await?;

    let parsed: ConfigToml = crate::user_settings::effective_settings(chaos_home.path())
        .await?
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
    crate::test_support::seed_user_settings_toml(
        chaos_home.path(),
        r#"
[profiles.dev]
model = "gordon"
model_reasoning_effort = "medium"

[profiles.prod]
model = "sherlock"
"#,
    )
    .await?;
    ConfigEditsBuilder::new(chaos_home.path())
        .with_profile(Some("dev"))
        .set_model(Some("o4-high"), Some(ReasoningEffort::Medium))
        .apply()
        .await?;

    let parsed: ConfigToml = crate::user_settings::effective_settings(chaos_home.path())
        .await?
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
