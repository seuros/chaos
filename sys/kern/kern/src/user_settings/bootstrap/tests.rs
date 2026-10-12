use super::*;

fn storage_env_is_clear() -> bool {
    std::env::var_os("CHAOS_STORAGE_URL").is_none()
        && std::env::var_os("CHAOS_SQLITE_HOME").is_none()
}

#[test]
fn missing_file_reads_as_empty_bootstrap() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    assert_eq!(
        BootstrapConfig::read(home.path())?,
        BootstrapConfig::default()
    );
    assert!(!BootstrapConfig::read(home.path())?.has_storage_url());
    Ok(())
}

#[test]
fn legacy_user_settings_in_bootstrap_file_are_rejected_with_a_hint() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        BootstrapConfig::path(home.path()),
        "model = \"gpt-5.1\"\nstorage_url = \"sqlite:///tmp/chaos.sqlite\"\n",
    )?;
    let error = BootstrapConfig::read(home.path()).unwrap_err().to_string();
    assert!(
        error.contains("`model` is not a bootstrap setting"),
        "{error}"
    );
    assert!(error.contains("chaos config set model"), "{error}");
    Ok(())
}

#[test]
fn removed_sqlite_home_key_is_rejected_with_a_hint() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        BootstrapConfig::path(home.path()),
        "sqlite_home = \"/var/lib/chaos\"\n",
    )?;
    let error = BootstrapConfig::read(home.path()).unwrap_err().to_string();
    assert!(error.contains("`sqlite_home` was removed"), "{error}");
    assert!(error.contains("CHAOS_SQLITE_HOME"), "{error}");
    Ok(())
}

#[test]
fn wrong_value_types_report_file_positions() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    std::fs::write(BootstrapConfig::path(home.path()), "storage_url = 42\n")?;
    let error = BootstrapConfig::read(home.path()).unwrap_err().to_string();
    assert!(error.contains("config.toml:1:"), "{error}");
    Ok(())
}

#[test]
fn resolve_requires_an_explicit_storage_choice() -> anyhow::Result<()> {
    if !storage_env_is_clear() {
        return Ok(());
    }
    let home = tempfile::tempdir()?;
    let error = BootstrapConfig::default()
        .resolve(home.path())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("No runtime storage is configured"),
        "{error}"
    );
    assert!(
        error.contains("chaos config bootstrap set storage_url"),
        "{error}"
    );
    Ok(())
}

#[test]
fn set_bootstrap_round_trips_both_keys_and_rejects_others() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    set_bootstrap(home.path(), "storage_url", "sqlite:///tmp/chaos.sqlite")?;
    set_bootstrap(
        home.path(),
        "egress_url",
        "https://gateway.example/egress/chaos",
    )?;
    let bootstrap = BootstrapConfig::read(home.path())?;
    assert_eq!(
        bootstrap.storage_url.as_deref(),
        Some("sqlite:///tmp/chaos.sqlite")
    );
    assert_eq!(
        bootstrap.egress_url.as_deref(),
        Some("https://gateway.example/egress/chaos")
    );
    assert!(set_bootstrap(home.path(), "model", "\"x\"").is_err());
    assert!(
        set_bootstrap(
            home.path(),
            "storage_url",
            "postgres://chaos:secret@db.example/chaos"
        )
        .is_err(),
        "credential-bearing URLs must use env: references"
    );

    let resolved = bootstrap.resolve(home.path())?;
    assert_eq!(resolved.storage_url, "sqlite:///tmp/chaos.sqlite");
    assert_eq!(
        resolved.egress_url.as_deref(),
        Some("https://gateway.example/egress/chaos")
    );
    assert_eq!(resolved.storage_home, home.path());
    Ok(())
}

#[test]
fn redaction_hides_userinfo_and_queries_but_keeps_references() {
    let bootstrap = BootstrapConfig {
        storage_url: Some("postgres://chaos:secret@db.example/chaos?sslmode=require".into()),
        egress_url: Some("env:CHAOS_EGRESS".into()),
    }
    .redacted();
    assert_eq!(
        bootstrap.storage_url.as_deref(),
        Some("postgres://REDACTED@db.example/chaos?REDACTED")
    );
    assert_eq!(bootstrap.egress_url.as_deref(), Some("env:CHAOS_EGRESS"));
}

#[test]
fn sqlite_in_home_matches_the_test_bootstrap_helper() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    crate::test_support::bootstrap_sqlite(home.path())?;
    let resolved = BootstrapConfig::read(home.path())?.resolve(home.path())?;
    assert_eq!(resolved, ResolvedBootstrap::sqlite_in(home.path()));
    Ok(())
}
