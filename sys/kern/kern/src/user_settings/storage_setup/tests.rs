use super::*;
use crate::user_settings::BootstrapConfig;

#[test]
fn fresh_home_needs_setup_without_creating_state() -> anyhow::Result<()> {
    let parent = tempfile::tempdir()?;
    let home = parent.path().join("new-machine");
    assert!(is_needed_with_environment(&home, false)?);
    assert!(!home.exists());
    assert!(!is_needed_with_environment(&home, true)?);
    Ok(())
}

#[test]
fn existing_configuration_and_databases_are_preserved() -> anyhow::Result<()> {
    for config in [
        "storage_url = 'postgresql://localhost/chaos'",
        "storage_url = 'env:UNAVAILABLE_DATABASE'",
        "storage_url = 'sqlite:///existing/chaos.sqlite'",
    ] {
        let home = tempfile::tempdir()?;
        std::fs::write(home.path().join("config.toml"), config)?;
        assert!(!is_needed_with_environment(home.path(), false)?);
    }
    let home = tempfile::tempdir()?;
    std::fs::write(home.path().join("chaos.sqlite"), [])?;
    assert!(!is_needed_with_environment(home.path(), false)?);
    Ok(())
}

#[test]
fn unrelated_local_identity_and_egress_do_not_skip_setup() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("installation-id"),
        uuid::Uuid::new_v4().to_string(),
    )?;
    std::fs::write(
        home.path().join("config.toml"),
        "egress_url = 'https://gateway.example/egress'",
    )?;
    assert!(is_needed_with_environment(home.path(), false)?);
    Ok(())
}

#[test]
fn malformed_bootstrap_is_not_treated_as_a_fresh_installation() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    std::fs::write(home.path().join("config.toml"), "[invalid")?;
    assert!(is_needed_with_environment(home.path(), false).is_err());
    Ok(())
}

#[test]
fn postgres_validation_requires_the_selected_backend_and_database() {
    for connection in [
        "",
        "not a url with secret-password",
        "sqlite::memory:",
        "https://user:secret-password@example.com/db",
        "postgresql://localhost",
        "env:",
    ] {
        let error = postgres_url(connection).expect_err("invalid PostgreSQL connection");
        assert!(!error.to_string().contains("secret-password"));
    }
    for connection in [
        "postgres://user:secret-password@localhost/chaos",
        "postgresql://localhost:5432/chaos?sslmode=require",
    ] {
        assert_eq!(postgres_url(connection).expect("valid URL"), connection);
    }
}

#[tokio::test]
async fn sqlite_setup_persists_choice_before_config_loading() -> anyhow::Result<()> {
    let parent = tempfile::tempdir()?;
    let home = parent.path().join("space # question? percent%");
    std::fs::create_dir(&home)?;
    std::fs::write(
        home.join("config.toml"),
        "egress_url = 'https://gateway.example/egress'",
    )?;
    configure(&home, StorageChoice::Sqlite).await?;
    assert!(home.join("chaos.sqlite").is_file());
    let bootstrap = BootstrapConfig::read(&home)?;
    assert!(
        bootstrap
            .storage_url
            .as_deref()
            .is_some_and(|url| url.starts_with("sqlite:///"))
    );
    assert_eq!(
        bootstrap.egress_url.as_deref(),
        Some("https://gateway.example/egress")
    );
    assert!(!is_needed_with_environment(&home, false)?);
    // Setup must not have pinned settings to a different database.
    crate::user_settings::snapshot(&home).await?;
    Ok(())
}

#[tokio::test]
async fn sqlite_setup_creates_a_missing_home() -> anyhow::Result<()> {
    let parent = tempfile::tempdir()?;
    let home = parent.path().join("new-home");
    configure(&home, StorageChoice::Sqlite).await?;
    assert!(home.join("chaos.sqlite").is_file());
    assert!(BootstrapConfig::read(&home)?.storage_url.is_some());
    Ok(())
}

#[tokio::test]
async fn invalid_postgres_does_not_save_or_fall_back_to_sqlite() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    assert!(
        configure(
            home.path(),
            StorageChoice::Postgres("sqlite::memory:".into())
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read_dir(home.path())?.count(), 0);
    assert!(is_needed_with_environment(home.path(), false)?);
    Ok(())
}

#[tokio::test]
async fn driver_errors_do_not_expose_credentials_or_save_configuration() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    // Invalid TLS mode fails inside the driver before any network connection.
    let error = configure(
        home.path(),
        StorageChoice::Postgres(
            "postgresql://user:secret-password@localhost/chaos?sslmode=secret-invalid-mode".into(),
        ),
    )
    .await
    .expect_err("driver must reject invalid TLS mode");
    assert!(!format!("{error:#}").contains("secret"));
    assert_eq!(std::fs::read_dir(home.path())?.count(), 0);
    assert!(is_needed_with_environment(home.path(), false)?);
    Ok(())
}

#[tokio::test]
async fn setup_does_not_overwrite_a_choice_made_elsewhere() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let config = "storage_url = 'postgresql://localhost/already-configured'";
    std::fs::write(home.path().join("config.toml"), config)?;
    assert!(configure(home.path(), StorageChoice::Sqlite).await.is_err());
    assert_eq!(
        std::fs::read_to_string(home.path().join("config.toml"))?,
        config
    );
    assert!(!home.path().join("chaos.sqlite").exists());
    Ok(())
}
