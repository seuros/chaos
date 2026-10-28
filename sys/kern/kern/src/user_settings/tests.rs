use super::*;

#[tokio::test]
async fn credential_bearing_mcp_urls_are_rejected_before_storage() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    crate::test_support::bootstrap_sqlite(home.path())?;
    let config = toml::from_str("url = 'https://user:secret@example.com/mcp'")?;
    assert!(
        crate::config::upsert_global_mcp_server(home.path(), "test", &config)
            .await
            .is_err()
    );
    assert!(!home.path().join("chaos.sqlite").exists());
    Ok(())
}

#[tokio::test]
async fn changing_storage_destination_requires_restart() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    crate::test_support::bootstrap_sqlite(home.path())?;
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
        "sqlite_home = '/var/lib/chaos'",
        "egress_url = 'https://gateway.example/egress'",
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
fn machine_warnings_are_validated_user_settings() {
    let home = tempfile::tempdir().unwrap();
    let settings = toml::from_str(
        "[machine_warnings]\nbattery_percent=8\ndisk_free_percent=3\n\
         disk_free_bytes_cap=1073741824",
    )
    .unwrap();
    assert!(validate(&settings, home.path()).is_ok());
    for setting in ["battery_percent=101", "disk_free_bytes_cap=0"] {
        let invalid = toml::from_str(&format!("[machine_warnings]\n{setting}")).unwrap();
        assert!(validate(&invalid, home.path()).is_err());
    }
}

#[test]
fn bootstrap_edits_are_validated_and_serialized() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    for (key, value) in [
        ("storage_url", "https://example.com/database"),
        ("egress_url", "postgres://localhost/database"),
        ("storage_url", "env:"),
        ("model", "\"x\""),
    ] {
        assert!(
            set_bootstrap(home.path(), key, value).is_err(),
            "{key}={value}"
        );
    }
    assert!(!BootstrapConfig::path(home.path()).exists());
    set_bootstrap(home.path(), "egress_url", "https://gateway.example/egress")?;
    assert_eq!(
        BootstrapConfig::read(home.path())?.egress_url.as_deref(),
        Some("https://gateway.example/egress")
    );
    // A legacy settings file is never silently truncated by a bootstrap edit.
    std::fs::write(BootstrapConfig::path(home.path()), "model = 'legacy'\n")?;
    assert!(set_bootstrap(home.path(), "egress_url", "https://gateway.example/egress").is_err());
    assert_eq!(
        std::fs::read_to_string(BootstrapConfig::path(home.path()))?,
        "model = 'legacy'\n"
    );
    Ok(())
}

#[tokio::test]
async fn database_user_restrictions_reads_policy_sources() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    crate::test_support::bootstrap_sqlite(home.path())?;
    assert!(
        database_user_restrictions(home.path())
            .await?
            .get_allowed_prefixes()
            .is_empty()
    );
    let runtime = open(home.path()).await?;
    let current = runtime.settings_snapshot().await?;
    let source = "prefix_rule {pattern = {'echo'}, decision = 'allow'}\n\
                  prefix_rule {pattern = {'rm'}, decision = 'forbidden'}\n";
    runtime
        .commit_settings_import(
            current.revision,
            &current.settings,
            None,
            &[],
            Some(&serde_json::json!([source])),
        )
        .await?;
    let policy = database_user_restrictions(home.path()).await?;
    assert!(
        policy.get_allowed_prefixes().is_empty(),
        "database policy sources can only restrict"
    );
    assert!(
        shell_policy(home.path(), home.path())
            .await?
            .get_allowed_prefixes()
            .is_empty()
    );
    let host = "host_executable {name = 'echo', path = '/bin/echo'}\n";
    runtime
        .commit_settings_import(
            current.revision + 1,
            &current.settings,
            None,
            &[],
            Some(&serde_json::json!([host])),
        )
        .await?;
    assert!(database_user_restrictions(home.path()).await.is_err());
    Ok(())
}

#[tokio::test]
async fn database_shell_grants_are_scoped_and_revocable() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    crate::test_support::bootstrap_sqlite(home.path())?;
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

#[tokio::test]
async fn a_running_process_keeps_storage_when_the_bootstrap_file_disappears() -> anyhow::Result<()>
{
    let home = tempfile::tempdir()?;
    crate::test_support::bootstrap_sqlite(home.path())?;
    snapshot(home.path()).await?;

    std::fs::remove_file(BootstrapConfig::path(home.path()))?;
    snapshot(home.path())
        .await
        .expect("storage resolved at startup stays usable");

    std::fs::write(BootstrapConfig::path(home.path()), "model = 'x'\n")?;
    assert!(
        snapshot(home.path()).await.is_err(),
        "an invalid bootstrap file still fails closed"
    );
    Ok(())
}

#[tokio::test]
async fn stale_rule_snapshot_cannot_overwrite_a_concurrent_import() -> anyhow::Result<()> {
    use crate::user_settings::rules;
    let home = tempfile::tempdir()?;
    crate::test_support::bootstrap_sqlite(home.path())?;
    let deny_rm = "prefix_rule {pattern = {'rm'}, decision = 'forbidden'}\n";
    let deny_ls = "prefix_rule {pattern = {'ls'}, decision = 'forbidden'}\n";

    // Writer B reads first; writer A then imports and commits.
    let stale = rules::snapshot(home.path()).await?;
    rules::put_user_rule(home.path(), "a", deny_rm).await?;

    // B commits its list built from the stale read: it must not win.
    let mut rules_from_b = stale.rules.clone();
    rules_from_b.push(rules::UserRule {
        name: "b".into(),
        source: deny_ls.into(),
    });
    let err = rules::commit(home.path(), &stale, &rules_from_b)
        .await
        .expect_err("stale revision must be rejected");
    assert!(err.to_string().contains("revision conflict"), "{err:#}");
    assert_eq!(
        rules::list_user_rules(home.path())
            .await?
            .iter()
            .map(|rule| rule.name.as_str())
            .collect::<Vec<_>>(),
        vec!["a"]
    );

    // The same race on removal: B's stale removal cannot drop A's rule.
    let stale = rules::snapshot(home.path()).await?;
    rules::put_user_rule(home.path(), "b", deny_ls).await?;
    assert!(rules::commit(home.path(), &stale, &[]).await.is_err());
    assert_eq!(rules::list_user_rules(home.path()).await?.len(), 2);
    Ok(())
}

#[tokio::test]
async fn stored_rules_round_trip_by_name() -> anyhow::Result<()> {
    use crate::user_settings::rules;
    let home = tempfile::tempdir()?;
    crate::test_support::bootstrap_sqlite(home.path())?;
    let deny_rm = "prefix_rule {pattern = {'rm'}, decision = 'forbidden'}\n";
    let deny_ls = "prefix_rule {pattern = {'ls'}, decision = 'forbidden'}\n";
    rules::put_user_rule(home.path(), "zeta", deny_rm).await?;
    rules::put_user_rule(home.path(), "alpha", deny_ls).await?;
    rules::put_user_rule(home.path(), "zeta", deny_ls).await?;
    let stored = rules::list_user_rules(home.path()).await?;
    assert_eq!(
        stored
            .iter()
            .map(|rule| (rule.name.as_str(), rule.source.as_str()))
            .collect::<Vec<_>>(),
        vec![("alpha", deny_ls), ("zeta", deny_ls)],
        "listing is sorted and a repeated name replaces the earlier document"
    );
    assert!(
        rules::remove_user_rule(home.path(), "missing")
            .await
            .is_err()
    );
    rules::remove_user_rule(home.path(), "alpha").await?;
    assert_eq!(rules::list_user_rules(home.path()).await?.len(), 1);
    assert!(
        rules::put_user_rule(home.path(), "bad name!", deny_rm)
            .await
            .is_err()
    );

    let parsed = rules::parse_policy_sources(&serde_json::json!([
        "prefix_rule {pattern = {'a'}, decision = 'forbidden'}",
        {"name": "named", "source": "prefix_rule {pattern = {'b'}, decision = 'forbidden'}"}
    ]))?;
    assert_eq!(
        parsed
            .iter()
            .map(|rule| rule.name.as_str())
            .collect::<Vec<_>>(),
        vec!["legacy-1", "named"]
    );
    Ok(())
}
