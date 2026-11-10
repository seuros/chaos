use super::*;
use pretty_assertions::assert_eq;

fn key(dir: &tempfile::TempDir) -> String {
    normalize_project_path(dir.path())
        .unwrap()
        .to_string_lossy()
        .to_string()
}

async fn stored(home: &Path, path: &str) -> Decision {
    list(home)
        .await
        .unwrap()
        .entries
        .into_iter()
        .find(|entry| entry.project_path == path)
        .map_or(Decision::None, |entry| entry.decision)
}

async fn sqlite_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    crate::test_support::bootstrap_sqlite(home.path()).unwrap();
    home
}

fn postgres_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn postgres_home(url: &str, installation_from: Option<&Path>) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    crate::user_settings::set_bootstrap(home.path(), "storage_url", url).unwrap();
    if let Some(source) = installation_from {
        std::fs::copy(
            source.join("installation-id"),
            home.path().join("installation-id"),
        )
        .unwrap();
    }
    home
}

#[tokio::test]
async fn set_and_revoke_are_compare_and_set() -> anyhow::Result<()> {
    let home = sqlite_home().await;
    let project = tempfile::tempdir()?;
    let path = key(&project);

    // Wrong expectation: refused, nothing written.
    assert!(
        prepare_set(
            home.path(),
            project.path(),
            Decision::Trusted,
            Decision::Untrusted
        )
        .await
        .is_err()
    );
    assert_eq!(stored(home.path(), &path).await, Decision::None);

    let receipt = prepare_set(
        home.path(),
        project.path(),
        Decision::Trusted,
        Decision::None,
    )
    .await?
    .commit()
    .await?;
    assert_eq!(receipt.format, RECEIPT_FORMAT);
    assert_eq!(receipt.actor, "operator-cli");
    assert_eq!(receipt.storage, "sqlite");
    assert_eq!(receipt.changes.len(), 1);
    assert_eq!(receipt.changes[0].before, Decision::None);
    assert_eq!(receipt.changes[0].after, Decision::Trusted);
    assert_eq!(stored(home.path(), &path).await, Decision::Trusted);

    // A stale "none" expectation after the grant is refused.
    assert!(
        prepare_set(
            home.path(),
            project.path(),
            Decision::Untrusted,
            Decision::None
        )
        .await
        .is_err()
    );

    prepare_set(
        home.path(),
        project.path(),
        Decision::None,
        Decision::Trusted,
    )
    .await?
    .commit()
    .await?;
    assert_eq!(stored(home.path(), &path).await, Decision::None);
    Ok(())
}

#[tokio::test]
async fn race_between_prepare_and_commit_fails_closed() -> anyhow::Result<()> {
    let home = sqlite_home().await;
    let project = tempfile::tempdir()?;
    let path = key(&project);
    let prepared = prepare_set(
        home.path(),
        project.path(),
        Decision::Trusted,
        Decision::None,
    )
    .await?;
    // The resident (or anyone) decides "untrusted" after the operator reviewed.
    crate::user_settings::open(home.path())
        .await?
        .set_project_trust(Path::new(&path), TrustLevel::Untrusted)
        .await?;
    let err = prepared.commit().await.unwrap_err().to_string();
    assert!(err.contains("project trust conflict"), "{err}");
    assert_eq!(stored(home.path(), &path).await, Decision::Untrusted);
    Ok(())
}

#[tokio::test]
async fn trust_set_through_admin_activates_project_hooks() -> anyhow::Result<()> {
    let home = sqlite_home().await;
    let project = tempfile::tempdir()?;
    // Register an approved project hook directly: the CLI path refuses agent shells,
    // and this suite may itself run inside one.
    let definition: chaos_ipc::hooks::HookDefinition = serde_json::from_value(serde_json::json!({
        "event": "before_turn", "command": "true",
        "project": crate::hooks::project_root(project.path())?,
    }))?;
    let installation = crate::user_settings::installation_id(home.path())?;
    crate::user_settings::open(home.path())
        .await?
        .commit_hook("project", 0, Some(&definition), true, &installation)
        .await?;
    let reason = |hooks: Vec<crate::hooks::HookView>| hooks[0].inactive_reason.clone();
    assert_eq!(
        reason(crate::hooks::list(home.path(), project.path()).await?).as_deref(),
        Some("project is not trusted")
    );
    prepare_set(
        home.path(),
        project.path(),
        Decision::Trusted,
        Decision::None,
    )
    .await?
    .commit()
    .await?;
    assert_eq!(
        reason(crate::hooks::list(home.path(), project.path()).await?),
        None
    );
    Ok(())
}

#[tokio::test]
async fn agent_shell_is_refused_even_with_yes() {
    let err = confirm_operator("change", true, true)
        .unwrap_err()
        .to_string();
    assert!(err.contains("agent shell"), "{err}");
    assert!(confirm_operator("change", true, false).is_ok());
}

#[tokio::test]
async fn scope_escapes_fail_closed() -> anyhow::Result<()> {
    let home = sqlite_home().await;
    let project = tempfile::tempdir()?;
    // Relative paths, the filesystem root, CHAOS_HOME itself and missing paths.
    assert!(normalize_project_path(Path::new("relative/dir")).is_err());
    assert!(normalize_project_path(Path::new("/")).is_err());
    assert!(
        prepare_set(home.path(), home.path(), Decision::Trusted, Decision::None)
            .await
            .is_err()
    );
    assert!(
        prepare_set(
            home.path(),
            &project.path().join("missing"),
            Decision::Trusted,
            Decision::None
        )
        .await
        .is_err()
    );
    // A manifest path that is not its own exact key is refused.
    let installation_id = crate::user_settings::installation_id(home.path())?;
    let manifest = Manifest {
        format: MANIFEST_FORMAT.to_string(),
        installation_id,
        entries: vec![ManifestEntry {
            project_path: format!("{}/./", key(&project)),
            decision: Decision::Trusted,
            expect: None,
        }],
    };
    let json = serde_json::to_string(&manifest)?;
    assert!(
        prepare_apply(home.path(), &json, &manifest.digest()?)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn manifest_is_bound_to_digest_installation_and_current_state() -> anyhow::Result<()> {
    let home = sqlite_home().await;
    let a = tempfile::tempdir()?;
    let b = tempfile::tempdir()?;
    let installation_id = crate::user_settings::installation_id(home.path())?;
    let mut manifest = Manifest {
        format: MANIFEST_FORMAT.to_string(),
        installation_id: installation_id.clone(),
        entries: vec![
            ManifestEntry {
                project_path: key(&a),
                decision: Decision::Trusted,
                expect: None,
            },
            ManifestEntry {
                project_path: key(&b),
                decision: Decision::Untrusted,
                expect: None,
            },
        ],
    };
    let json = serde_json::to_string(&manifest)?;

    // Digest of a different review: refused.
    assert!(prepare_apply(home.path(), &json, "v1:0000").await.is_err());

    // Another installation's manifest: refused.
    let mut foreign = manifest.clone();
    foreign.installation_id = uuid::Uuid::new_v4().to_string();
    let foreign_json = serde_json::to_string(&foreign)?;
    let err = prepare_apply(home.path(), &foreign_json, &foreign.digest()?)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("do not cross installations"), "{err}");

    // A resident already chose differently for `a`: the whole batch is refused.
    crate::user_settings::open(home.path())
        .await?
        .set_project_trust(Path::new(&key(&a)), TrustLevel::Untrusted)
        .await?;
    let err = prepare_apply(home.path(), &json, &manifest.digest()?)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("nothing was changed"), "{err}");
    assert_eq!(stored(home.path(), &key(&b)).await, Decision::None);

    // With an explicit expectation matching what the operator saw, it applies.
    manifest.entries[0].expect = Some(Decision::Untrusted);
    let json = serde_json::to_string(&manifest)?;
    let receipt = prepare_apply(home.path(), &json, &manifest.digest()?)
        .await?
        .commit()
        .await?;
    assert_eq!(receipt.manifest_digest, Some(manifest.digest()?));
    assert_eq!(receipt.changes.len(), 2);
    assert_eq!(stored(home.path(), &key(&a)).await, Decision::Trusted);
    assert_eq!(stored(home.path(), &key(&b)).await, Decision::Untrusted);

    // Retrying the same reviewed manifest is now refused for `a` (expected untrusted),
    // not silently replayed.
    assert!(
        prepare_apply(home.path(), &json, &manifest.digest()?)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn export_then_apply_is_idempotent_on_same_storage() -> anyhow::Result<()> {
    let home = sqlite_home().await;
    let a = tempfile::tempdir()?;
    prepare_set(home.path(), a.path(), Decision::Untrusted, Decision::None)
        .await?
        .commit()
        .await?;
    let manifest = export(home.path()).await?;
    let json = serde_json::to_string(&manifest)?;
    let receipt = prepare_apply(home.path(), &json, &manifest.digest()?)
        .await?
        .commit()
        .await?;
    assert!(receipt.changes.is_empty());
    assert_eq!(receipt.unchanged, vec![key(&a)]);
    Ok(())
}

/// Same installation moving from SQLite to PostgreSQL: granted AND denied decisions
/// arrive; nothing else does.
#[tokio::test]
async fn sqlite_to_postgres_migration_preserves_granted_and_denied() -> anyhow::Result<()> {
    let Some(url) = postgres_url() else {
        tracing::warn!("skipping postgres trust migration; TEST_DATABASE_URL not set");
        return Ok(());
    };
    let source = sqlite_home().await;
    let trusted = tempfile::tempdir()?;
    let denied = tempfile::tempdir()?;
    prepare_set(
        source.path(),
        trusted.path(),
        Decision::Trusted,
        Decision::None,
    )
    .await?
    .commit()
    .await?;
    prepare_set(
        source.path(),
        denied.path(),
        Decision::Untrusted,
        Decision::None,
    )
    .await?
    .commit()
    .await?;
    let manifest = export(source.path()).await?;
    assert_eq!(manifest.entries.len(), 2);

    // A different installation on Postgres refuses the manifest.
    let stranger = postgres_home(&url, None);
    let json = serde_json::to_string(&manifest)?;
    assert!(
        prepare_apply(stranger.path(), &json, &manifest.digest()?)
            .await
            .is_err()
    );

    // The same installation on Postgres accepts it.
    let destination = postgres_home(&url, Some(source.path()));
    let receipt = prepare_apply(destination.path(), &json, &manifest.digest()?)
        .await?
        .commit()
        .await?;
    assert_eq!(receipt.storage, "postgres");
    assert_eq!(receipt.changes.len(), 2);
    assert_eq!(
        stored(destination.path(), &key(&trusted)).await,
        Decision::Trusted
    );
    assert_eq!(
        stored(destination.path(), &key(&denied)).await,
        Decision::Untrusted
    );

    // Retry is a no-op; a resident's later choice makes a stale replay refuse.
    let receipt = prepare_apply(destination.path(), &json, &manifest.digest()?)
        .await?
        .commit()
        .await?;
    assert!(receipt.changes.is_empty());
    prepare_set(
        destination.path(),
        denied.path(),
        Decision::Trusted,
        Decision::Untrusted,
    )
    .await?
    .commit()
    .await?;
    assert!(
        prepare_apply(destination.path(), &json, &manifest.digest()?)
            .await
            .is_err()
    );
    // Clean the shared database.
    for dir in [&trusted, &denied] {
        let found = stored(destination.path(), &key(dir)).await;
        prepare_set(destination.path(), dir.path(), Decision::None, found)
            .await?
            .commit()
            .await?;
    }
    Ok(())
}
