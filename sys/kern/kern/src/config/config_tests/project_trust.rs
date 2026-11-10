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

#[tokio::test]
async fn onboarding_trust_audit_names_the_installation() -> anyhow::Result<()> {
    let chaos_home = TempDir::new()?;
    crate::test_support::bootstrap_sqlite(chaos_home.path())?;
    let installation = crate::user_settings::installation_id(chaos_home.path())?;
    let project_dir = chaos_home.path().join("project");
    tokio::fs::create_dir_all(&project_dir).await?;

    set_project_trust_level(chaos_home.path(), &project_dir, TrustLevel::Trusted)?;

    let db = crate::user_settings::open(chaos_home.path()).await?;
    let crate::runtime_db::RuntimeDbHandle::Sqlite(runtime) = db else {
        anyhow::bail!("expected sqlite");
    };
    let subject: String = sqlx::query_scalar(
        "SELECT subject FROM configuration_events WHERE event = 'project_trust_trusted'",
    )
    .fetch_one(runtime.pool())
    .await?;
    let subject: serde_json::Value = serde_json::from_str(&subject)?;
    assert_eq!(subject["actor"], "onboarding");
    assert_eq!(subject["installation_id"], installation.as_str());
    assert_eq!(subject["before"], "none");

    // An unreadable identity fails the write rather than auditing none.
    std::fs::write(chaos_home.path().join("installation-id"), "not-a-uuid")?;
    assert!(set_project_trust_level(chaos_home.path(), &project_dir, TrustLevel::Untrusted).is_err());
    Ok(())
}
