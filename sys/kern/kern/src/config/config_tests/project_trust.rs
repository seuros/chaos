use super::*;

#[tokio::test]
async fn set_project_trust_level_persists_to_runtime_db() -> anyhow::Result<()> {
    let chaos_home = TempDir::new()?;
    crate::test_support::bootstrap_sqlite(chaos_home.path())?;
    let project_dir = chaos_home.path().join("project");
    tokio::fs::create_dir_all(&project_dir).await?;

    set_project_trust_level(chaos_home.path(), &project_dir, TrustLevel::Trusted)?;

    let bootstrap = crate::user_settings::ResolvedBootstrap::sqlite_in(chaos_home.path());
    let runtime = crate::runtime_db::open_or_create_runtime_db(
        &bootstrap.storage_url,
        chaos_home.path(),
        "test-provider",
    )
    .await?;
    let trust = runtime
        .get_project_trust(crate::runtime_db::normalize_cwd_for_runtime_db(&project_dir).as_path())
        .await?;
    assert_eq!(trust, Some(TrustLevel::Trusted));
    Ok(())
}

#[tokio::test]
async fn set_project_trust_level_respects_bootstrap_storage_url() -> anyhow::Result<()> {
    let chaos_home = TempDir::new()?;
    let state_dir = chaos_home.path().join("state");
    tokio::fs::create_dir_all(&state_dir).await?;
    let storage_url = format!("sqlite://{}", state_dir.join("chaos.sqlite").display());
    crate::user_settings::set_bootstrap(chaos_home.path(), "storage_url", &storage_url)?;
    let project_dir = chaos_home.path().join("project");
    tokio::fs::create_dir_all(&project_dir).await?;

    set_project_trust_level(chaos_home.path(), &project_dir, TrustLevel::Untrusted)?;

    assert!(state_dir.join("chaos.sqlite").exists());
    assert!(!chaos_home.path().join("chaos.sqlite").exists());
    let runtime = crate::runtime_db::open_or_create_runtime_db(
        &storage_url,
        chaos_home.path(),
        "test-provider",
    )
    .await?;
    let trust = runtime
        .get_project_trust(crate::runtime_db::normalize_cwd_for_runtime_db(&project_dir).as_path())
        .await?;
    assert_eq!(trust, Some(TrustLevel::Untrusted));
    Ok(())
}
