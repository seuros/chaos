use super::*;
use crate::user_settings::BootstrapConfig;

#[test]
fn fresh_home_needs_setup_without_creating_state() -> anyhow::Result<()> {
    let parent = tempfile::tempdir()?;
    let home = parent.path().join("new-machine");
    assert!(is_needed(&home)?);
    assert!(!home.exists());
    Ok(())
}

#[test]
fn environment_does_not_replace_toml_storage_choice() -> anyhow::Result<()> {
    // Set environment only in a subprocess, not in this parallel test harness.
    let home = tempfile::tempdir()?;
    for url in ["", "postgresql://localhost/environment-only"] {
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "user_settings::storage_setup::tests::fresh_home_needs_setup_without_creating_state",
            ])
            .env("CHAOS_STORAGE_URL", url)
            .env("CHAOS_DATABASE_URL", url)
            .env("CHAOS_SQLITE_HOME", home.path())
            .output()?;
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    Ok(())
}

#[test]
fn explicit_toml_storage_choices_skip_setup_without_resolving_references() -> anyhow::Result<()> {
    for config in [
        "storage_url = 'postgresql://localhost/chaos'",
        "storage_url = 'postgres://user:password@localhost/chaos'",
        "storage_url = 'env:CHAOS_DATABASE_URL'",
        "storage_url = 'env:UNAVAILABLE_DATABASE'",
        "storage_url = 'keyring:chaos-settings/00000000-0000-4000-8000-000000000000'",
        "storage_url = 'sqlite:///existing/chaos.sqlite'",
    ] {
        let home = tempfile::tempdir()?;
        std::fs::write(home.path().join("config.toml"), config)?;
        assert!(!is_needed(home.path())?);
        assert_eq!(std::fs::read_dir(home.path())?.count(), 1);
    }
    Ok(())
}

#[test]
fn local_database_without_toml_storage_choice_still_needs_setup() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let database = home.path().join("chaos.sqlite");
    std::fs::write(&database, b"existing database must not be touched")?;
    assert!(is_needed(home.path())?);
    assert_eq!(
        std::fs::read(&database)?,
        b"existing database must not be touched"
    );
    Ok(())
}

#[test]
fn empty_toml_storage_choice_needs_setup() -> anyhow::Result<()> {
    for config in ["", "storage_url = ''", "storage_url = '   '"] {
        let home = tempfile::tempdir()?;
        std::fs::write(home.path().join("config.toml"), config)?;
        assert!(is_needed(home.path())?);
        assert_eq!(
            std::fs::read_to_string(home.path().join("config.toml"))?,
            config
        );
    }
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
    assert!(is_needed(home.path())?);
    Ok(())
}

#[test]
fn malformed_bootstrap_is_not_treated_as_a_fresh_installation() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    for config in ["[invalid", "storage_url = 42", "storage_url = []"] {
        std::fs::write(home.path().join("config.toml"), config)?;
        assert!(is_needed(home.path()).is_err());
    }
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

#[test]
fn unavailable_keyring_obeys_platform_policy() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    for existing in [false, true] {
        let home = tempfile::tempdir()?;
        let config_path = home.path().join("config.toml");
        if existing {
            std::fs::write(
                &config_path,
                "egress_url = 'https://gateway.example/egress'",
            )?;
            std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o644))?;
        }
        let before = read_toml(home.path())?;
        let url = "postgresql://user:private-password@localhost/chaos";
        let result = persist_choice(
            home.path(),
            before.clone(),
            url.into(),
            Some(url.into()),
            |_| anyhow::bail!("keyring unavailable: private-driver-detail"),
        );
        #[cfg(target_os = "freebsd")]
        {
            result?;
            let bootstrap = BootstrapConfig::read(home.path())?;
            assert_eq!(bootstrap.resolved_storage_url()?.as_deref(), Some(url));
            assert_eq!(
                std::fs::metadata(&config_path)?.permissions().mode() & 0o777,
                0o600
            );
            assert!(!toml::to_string(&bootstrap.redacted())?.contains("private-password"));
            assert!(!is_needed(home.path())?);
            assert_eq!(std::fs::read_dir(home.path())?.count(), 2); // config + existing migration lock
            if existing {
                assert_eq!(read_toml(home.path())?["egress_url"], before["egress_url"]);
            }
        }
        #[cfg(not(target_os = "freebsd"))]
        {
            let error = result.unwrap_err();
            assert!(error.to_string().contains("env:VARIABLE"));
            assert!(!format!("{error:#}").contains("private"));
            assert_eq!(read_toml(home.path())?, before);
        }
        assert!(!home.path().join("chaos.sqlite").exists());
    }
    Ok(())
}

#[test]
fn available_keyring_is_preferred_to_literal_config() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let url = "postgresql://user:private-password@localhost/chaos";
    let reference = format!("keyring:chaos-settings/{}", uuid::Uuid::new_v4());
    persist_choice(
        home.path(),
        read_toml(home.path())?,
        url.into(),
        Some(url.into()),
        |value| {
            assert_eq!(value, url);
            Ok(reference.clone())
        },
    )?;
    assert_eq!(
        BootstrapConfig::read(home.path())?.storage_url,
        Some(reference)
    );
    assert!(
        !std::fs::read_to_string(home.path().join("config.toml"))?.contains("private-password")
    );
    Ok(())
}

#[test]
fn existing_references_are_saved_without_accessing_keyring() -> anyhow::Result<()> {
    for reference in [
        "env:CHAOS_DATABASE_URL".to_owned(),
        format!("keyring:chaos-settings/{}", uuid::Uuid::new_v4()),
    ] {
        let home = tempfile::tempdir()?;
        persist_choice(
            home.path(),
            read_toml(home.path())?,
            "postgresql://user:private-password@localhost/chaos".into(),
            Some(reference.clone()),
            |_| panic!("references must not be re-saved as literals"),
        )?;
        assert_eq!(
            BootstrapConfig::read(home.path())?.storage_url,
            Some(reference)
        );
        assert!(
            !std::fs::read_to_string(home.path().join("config.toml"))?.contains("private-password")
        );
    }
    Ok(())
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
    assert!(!is_needed(&home)?);
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
async fn sqlite_setup_replaces_an_empty_storage_choice() -> anyhow::Result<()> {
    for config in ["storage_url = ''", "storage_url = '   '"] {
        let home = tempfile::tempdir()?;
        std::fs::write(home.path().join("config.toml"), config)?;
        assert!(is_needed(home.path())?);
        configure(home.path(), StorageChoice::Sqlite).await?;
        assert!(!is_needed(home.path())?);
        assert!(home.path().join("chaos.sqlite").is_file());
        crate::user_settings::snapshot(home.path()).await?;
    }
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
    assert!(is_needed(home.path())?);
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
    assert!(is_needed(home.path())?);
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
