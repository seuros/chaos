use super::*;

#[tokio::test]
async fn credential_bearing_mcp_urls_are_rejected_before_storage() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let config = toml::from_str("url = 'https://user:secret@example.com/mcp'")?;
    assert!(
        crate::config::upsert_global_mcp_server(home.path(), "test", &config)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_dir(home.path())?.count(), 0);
    Ok(())
}

#[tokio::test]
async fn rules_only_installation_requires_migration_and_ignores_later_file_edits()
-> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    std::fs::create_dir(home.path().join("rules"))?;
    let rules = home.path().join("rules/user.decrees");
    std::fs::write(
        &rules,
        "prefix_rule {pattern = {'echo'}, decision = 'allow'}\nprefix_rule {pattern = {'rm'}, decision = 'forbidden'}\n",
    )?;
    assert!(snapshot(home.path()).await.is_err());
    let report = migrate(home.path(), false).await?;
    assert_eq!(report.grants_requiring_reapproval, 1);
    snapshot(home.path()).await?;
    let runtime = open(home.path()).await?;
    let imported = runtime.user_policy_sources().await?;
    std::fs::write(rules, "invalid policy")?;
    assert_eq!(runtime.user_policy_sources().await?, imported);
    assert!(
        shell_policy(home.path(), home.path())
            .await?
            .get_allowed_prefixes()
            .is_empty()
    );
    assert!(migrated_user_restrictions(home.path()).await?.is_some());
    Ok(())
}

#[tokio::test]
async fn changing_storage_destination_requires_restart() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    snapshot(home.path()).await?;
    set_bootstrap(
        home.path(),
        "storage_url",
        "sqlite:///unavailable/other.sqlite",
    )?;
    let error = snapshot(home.path()).await.unwrap_err();
    assert!(error.to_string().contains("restart"));
    Ok(())
}

#[test]
fn canonical_identity_is_order_independent_but_sensitive_to_changes() -> anyhow::Result<()> {
    let a: serde_json::Value =
        serde_json::from_str(r#"{"url":"https://example.com","env":{"B":2,"A":1}}"#)?;
    let b: serde_json::Value =
        serde_json::from_str(r#"{"env":{"A":1,"B":2},"url":"https://example.com"}"#)?;
    assert_eq!(fingerprint(&a)?, fingerprint(&b)?);
    let mut changed = b;
    changed["url"] = serde_json::json!("https://other.example.com");
    assert_ne!(fingerprint(&a)?, fingerprint(&changed)?);
    Ok(())
}

#[test]
fn settings_reject_unknown_bootstrap_and_positive_grants() {
    let home = tempfile::tempdir().expect("home");
    for text in [
        "not_a_setting = true",
        "storage_url = 'sqlite::memory:'",
        "[profiles.test]\nstorage_url = 'sqlite::memory:'",
        "[mcp_tool_approvals.test.tools]\nexecute = 'approve'",
        "[model_providers.custom]\nname='custom'\nexperimental_bearer_token='plaintext'",
    ] {
        assert!(
            validate(&toml::from_str(text).expect("TOML"), home.path()).is_err(),
            "{text}"
        );
    }
    assert!(
        validate(
            &toml::from_str("model = 'test'").expect("TOML"),
            home.path()
        )
        .is_ok()
    );
}

#[test]
fn project_configuration_cannot_grant_authority() {
    for text in [
        "storage_url = 'postgres://attacker/db'",
        "approval_policy = 'headless'",
        "[profiles.test]\nsandbox_mode = 'root-access'",
    ] {
        assert!(validate_project(&toml::from_str(text).expect("TOML")).is_err());
    }
    assert!(validate_project(&toml::from_str("model = 'test'").expect("TOML")).is_ok());
}

#[tokio::test]
async fn dry_run_does_not_write_and_migration_requires_reapproval() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let file = home.path().join("config.toml");
    let source = "model = 'test'\n[mcp_tool_approvals.skynet.tools]\nselect_project = 'approve'\nfleet = 'approve'\n";
    std::fs::write(&file, source)?;
    assert!(snapshot(home.path()).await.is_err());
    let report = migrate(home.path(), true).await?;
    assert_eq!(report.grants_requiring_reapproval, 2);
    assert!(!report.completed);
    assert_eq!(std::fs::read_dir(home.path())?.count(), 1);
    assert_eq!(std::fs::read_to_string(&file)?, source);
    // Recovery edits must not compete with migration or leave unusable input.
    let lock = lock_bootstrap(home.path())?;
    assert!(set_bootstrap(home.path(), "egress_url", "https://gateway.example/egress").is_err());
    assert!(migrate(home.path(), false).await.is_err());
    assert_eq!(std::fs::read_to_string(&file)?, source);
    drop(lock);
    for (key, value) in [
        ("storage_url", "https://example.com/database"),
        ("egress_url", "postgres://localhost/database"),
        ("storage_url", "env:"),
    ] {
        assert!(set_bootstrap(home.path(), key, value).is_err());
    }
    assert_eq!(std::fs::read_to_string(&file)?, source);
    assert!(migrate(home.path(), false).await?.completed);
    let settings = snapshot(home.path()).await?;
    assert_eq!(settings.settings["model"].as_str(), Some("test"));
    let runtime = open(home.path()).await?;
    let grants = runtime
        .list_approvals(&installation_id(home.path())?)
        .await?;
    assert_eq!(grants.len(), 2);
    assert!(
        grants
            .iter()
            .all(|grant| grant.state == ApprovalState::PendingReapproval)
    );
    let migrated_file = std::fs::read_to_string(&file)?;
    chaos_sysctl::edit::ConfigEditsBuilder::new(home.path())
        .set_model(Some("updated"), None)
        .apply()
        .await?;
    assert_eq!(std::fs::read_to_string(&file)?, migrated_file);
    assert_eq!(
        snapshot(home.path()).await?.settings["model"].as_str(),
        Some("updated")
    );
    assert!(migrate(home.path(), false).await?.completed);
    assert_eq!(
        snapshot(home.path()).await?.settings["model"].as_str(),
        Some("updated")
    );
    set_bootstrap(home.path(), "egress_url", "https://gateway.example/egress")?;
    assert_eq!(
        BootstrapConfig::read(home.path())?.egress_url.as_deref(),
        Some("https://gateway.example/egress")
    );
    assert_eq!(
        snapshot(home.path()).await?.settings["model"].as_str(),
        Some("updated")
    );
    Ok(())
}

#[tokio::test]
async fn interrupted_cleanup_does_not_overwrite_newer_settings() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let file = home.path().join("config.toml");
    let legacy = "model='old'\n";
    std::fs::write(&file, legacy)?;
    migrate(home.path(), false).await?;
    let runtime = open(home.path()).await?;
    let current = runtime.settings_snapshot().await?;
    runtime
        .commit_settings(current.revision, &serde_json::json!({"model":"new"}), None)
        .await?;
    std::fs::write(&file, legacy)?;
    migrate(home.path(), false).await?;
    assert_eq!(
        snapshot(home.path()).await?.settings["model"].as_str(),
        Some("new")
    );
    std::fs::write(&file, "model='different'\n")?;
    assert!(migrate(home.path(), false).await.is_err());
    Ok(())
}

#[tokio::test]
async fn database_shell_grants_are_scoped_and_revocable() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let a = tempfile::tempdir()?;
    let b = tempfile::tempdir()?;
    put_scoped_approval(
        home.path(),
        a.path(),
        "shell",
        serde_json::json!({"prefix":["cargo","test"]}),
    )
    .await?;
    assert_eq!(
        shell_policy(home.path(), a.path())
            .await?
            .get_allowed_prefixes()
            .len(),
        1
    );
    assert!(
        shell_policy(home.path(), b.path())
            .await?
            .get_allowed_prefixes()
            .is_empty()
    );
    open(home.path())
        .await?
        .revoke_approvals(&installation_id(home.path())?, None)
        .await?;
    assert!(
        shell_policy(home.path(), a.path())
            .await?
            .get_allowed_prefixes()
            .is_empty()
    );
    Ok(())
}

#[test]
fn installation_identity_is_local_stable_and_private() -> anyhow::Result<()> {
    let a = tempfile::tempdir()?;
    let b = tempfile::tempdir()?;
    let first = installation_id(a.path())?;
    assert_eq!(first, installation_id(a.path())?);
    assert_ne!(first, installation_id(b.path())?);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(a.path().join("installation-id"))?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    Ok(())
}
