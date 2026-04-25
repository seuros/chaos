use super::*;

#[tokio::test]
async fn set_project_trust_level_persists_to_runtime_db() -> anyhow::Result<()> {
    let chaos_home = TempDir::new()?;
    let project_dir = chaos_home.path().join("project");
    tokio::fs::create_dir_all(&project_dir).await?;

    set_project_trust_level(chaos_home.path(), &project_dir, TrustLevel::Trusted)?;

    let runtime =
        crate::runtime_db::open_or_create_runtime_db(chaos_home.path(), "test-provider").await?;
    let trust = runtime
        .get_project_trust(crate::runtime_db::normalize_cwd_for_runtime_db(&project_dir).as_path())
        .await?;
    assert_eq!(trust, Some(TrustLevel::Trusted));
    Ok(())
}

#[tokio::test]
async fn set_project_trust_level_respects_configured_sqlite_home() -> anyhow::Result<()> {
    let chaos_home = TempDir::new()?;
    let sqlite_home = chaos_home.path().join("state");
    tokio::fs::create_dir_all(&sqlite_home).await?;
    tokio::fs::write(
        chaos_home.path().join(CONFIG_TOML_FILE),
        format!("sqlite_home = {sqlite_home:?}"),
    )
    .await?;
    let project_dir = chaos_home.path().join("project");
    tokio::fs::create_dir_all(&project_dir).await?;

    set_project_trust_level(chaos_home.path(), &project_dir, TrustLevel::Untrusted)?;

    let runtime =
        crate::runtime_db::open_or_create_runtime_db(&sqlite_home, "test-provider").await?;
    let trust = runtime
        .get_project_trust(crate::runtime_db::normalize_cwd_for_runtime_db(&project_dir).as_path())
        .await?;
    assert_eq!(trust, Some(TrustLevel::Untrusted));
    Ok(())
}
