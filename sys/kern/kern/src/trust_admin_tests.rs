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

/// A home on the test database. The bootstrap names the environment variable,
/// not the URL: credential-bearing URLs are refused in the bootstrap (and CI's
/// carries a password).
fn postgres_home(_url: &str, installation_from: Option<&Path>) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    crate::user_settings::set_bootstrap(home.path(), "storage_url", "env:TEST_DATABASE_URL")
        .unwrap();
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
    .commit("test-operator")
    .await?;
    assert_eq!(receipt.format, RECEIPT_FORMAT);
    assert_eq!(receipt.actor, "test-operator");
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

    // "none" is never a decision anyone can write.
    let err = prepare_set(
        home.path(),
        project.path(),
        Decision::None,
        Decision::Trusted,
    )
    .await
    .unwrap_err();
    let err = format!("{err:#}");
    assert!(err.contains("cannot be set to none"), "{err}");

    // Revocation (what `chaos trust revoke` prepares) stores a denial.
    prepare_set(
        home.path(),
        project.path(),
        Decision::Untrusted,
        Decision::Trusted,
    )
    .await?
    .commit("test-operator")
    .await?;
    assert_eq!(stored(home.path(), &path).await, Decision::Untrusted);
    Ok(())
}

/// Mira's round-1 replay: grant A, export, revoke A, apply the same export and
/// digest. The revocation must hold.
#[tokio::test]
async fn stale_export_cannot_restore_revoked_trust() -> anyhow::Result<()> {
    let home = sqlite_home().await;
    let a = tempfile::tempdir()?;
    prepare_set(home.path(), a.path(), Decision::Trusted, Decision::None)
        .await?
        .commit("test-operator")
        .await?;
    let manifest = export(home.path()).await?;
    let json = serde_json::to_string(&manifest)?;
    prepare_set(
        home.path(),
        a.path(),
        Decision::Untrusted,
        Decision::Trusted,
    )
    .await?
    .commit("test-operator")
    .await?;
    let err = prepare_apply(home.path(), &json, &manifest.digest()?)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("nothing was changed"), "{err}");
    assert_eq!(stored(home.path(), &key(&a)).await, Decision::Untrusted);

    // A hand-written manifest cannot delete the denial either.
    let mut forget = manifest.clone();
    forget.entries[0].decision = Decision::None;
    forget.entries[0].expect = Some(Decision::Untrusted);
    let forget_json = serde_json::to_string(&forget)?;
    let err = prepare_apply(home.path(), &forget_json, &forget.digest()?)
        .await
        .unwrap_err();
    let err = format!("{err:#}");
    assert!(err.contains("cannot be set to none"), "{err}");
    assert_eq!(stored(home.path(), &key(&a)).await, Decision::Untrusted);
    Ok(())
}

/// A reviewed batch where `a` is already trusted (unchanged) and `b` is new.
async fn prepare_mixed_batch(
    home: &Path,
    a: &tempfile::TempDir,
    b: &tempfile::TempDir,
) -> anyhow::Result<PreparedTrustChange> {
    let manifest = Manifest {
        format: MANIFEST_FORMAT.to_string(),
        installation_id: crate::user_settings::installation_id(home)?,
        entries: vec![
            ManifestEntry {
                project_path: key(a),
                decision: Decision::Trusted,
                expect: Some(Decision::Trusted),
            },
            ManifestEntry {
                project_path: key(b),
                decision: Decision::Trusted,
                expect: Some(Decision::None),
            },
        ],
    };
    let prepared = prepare_apply(
        home,
        &serde_json::to_string(&manifest)?,
        &manifest.digest()?,
    )
    .await?;
    assert_eq!(prepared.unchanged, vec![key(a)]);
    assert_eq!(prepared.changes.len(), 1);
    Ok(prepared)
}

/// Every reviewed entry is re-checked at commit, including unchanged ones, in
/// a mixed batch and in a batch that changes nothing at all.
async fn unchanged_entries_are_rechecked(home: &Path) -> anyhow::Result<()> {
    let a = tempfile::tempdir()?;
    let b = tempfile::tempdir()?;
    prepare_set(home, a.path(), Decision::Trusted, Decision::None)
        .await?
        .commit("test-operator")
        .await?;

    // Mixed: `a` revoked after review; granting `b` must not go through.
    let prepared = prepare_mixed_batch(home, &a, &b).await?;
    crate::user_settings::open(home)
        .await?
        .set_project_trust(Path::new(&key(&a)), TrustLevel::Untrusted, None)
        .await?;
    let err = prepared
        .commit("test-operator")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("project trust conflict"), "{err}");
    assert_eq!(stored(home, &key(&b)).await, Decision::None);

    // All no-op: the receipt may not claim `a` is untrusted once it is not.
    let manifest = Manifest {
        format: MANIFEST_FORMAT.to_string(),
        installation_id: crate::user_settings::installation_id(home)?,
        entries: vec![ManifestEntry {
            project_path: key(&a),
            decision: Decision::Untrusted,
            expect: None,
        }],
    };
    let prepared = prepare_apply(
        home,
        &serde_json::to_string(&manifest)?,
        &manifest.digest()?,
    )
    .await?;
    assert!(prepared.changes.is_empty());
    crate::user_settings::open(home)
        .await?
        .set_project_trust(Path::new(&key(&a)), TrustLevel::Trusted, None)
        .await?;
    let err = prepared
        .commit("test-operator")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("project trust conflict"), "{err}");
    Ok(())
}

#[tokio::test]
async fn unchanged_entries_are_rechecked_at_commit_sqlite() -> anyhow::Result<()> {
    unchanged_entries_are_rechecked(sqlite_home().await.path()).await
}

async fn unchanged_entries_are_rechecked_at_commit_postgres() -> anyhow::Result<()> {
    let Some(url) = postgres_url() else {
        tracing::warn!("skipping postgres trust recheck; TEST_DATABASE_URL not set");
        return Ok(());
    };
    unchanged_entries_are_rechecked(postgres_home(&url, None).path()).await
}

/// A concurrent writer that changes an unchanged entry while the batch is
/// committing: the batch must wait for it and then refuse, not commit `b`
/// against the old snapshot of `a` (what a plain READ COMMITTED SELECT would do).
async fn postgres_unchanged_entry_is_locked_against_concurrent_change() -> anyhow::Result<()> {
    let Some(url) = postgres_url() else {
        tracing::warn!("skipping postgres trust lock test; TEST_DATABASE_URL not set");
        return Ok(());
    };
    let home = postgres_home(&url, None);
    let a = tempfile::tempdir()?;
    let b = tempfile::tempdir()?;
    prepare_set(home.path(), a.path(), Decision::Trusted, Decision::None)
        .await?
        .commit("test-operator")
        .await?;
    let prepared = prepare_mixed_batch(home.path(), &a, &b).await?;

    let pool = sqlx::PgPool::connect(&url).await?;
    let mut other = pool.begin().await?;
    let writer: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *other)
        .await?;
    sqlx::query("UPDATE project_trust SET trust_level = 'untrusted' WHERE project_path = $1")
        .bind(key(&a))
        .execute(&mut *other)
        .await?;
    let commit = tokio::spawn(prepared.commit("test-operator"));

    // Positive evidence, not a sleep: a backend running a project_trust query is
    // blocked by exactly this writer. Without the row lock the batch never
    // blocks, so this wait fails regardless of scheduling.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let blocked = loop {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
             WHERE $1 = ANY(pg_blocking_pids(pid)) AND query LIKE '%project_trust%'",
        )
        .bind(writer)
        .fetch_one(&pool)
        .await?;
        if waiting > 0 {
            break true;
        }
        if commit.is_finished() || tokio::time::Instant::now() >= deadline {
            break false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    let cleanup = || async {
        sqlx::query("DELETE FROM project_trust WHERE project_path = ANY($1)")
            .bind(vec![key(&a), key(&b)])
            .execute(&pool)
            .await
    };
    if !blocked {
        other.rollback().await?;
        let _ = commit.await;
        cleanup().await?;
        panic!("the batch never waited on the concurrent writer's row lock");
    }
    other.commit().await?;
    let err = commit.await?.unwrap_err().to_string();
    assert!(err.contains("project trust conflict"), "{err}");
    assert_eq!(stored(home.path(), &key(&b)).await, Decision::None);
    cleanup().await?;
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
        .set_project_trust(Path::new(&path), TrustLevel::Untrusted, None)
        .await?;
    let err = prepared
        .commit("test-operator")
        .await
        .unwrap_err()
        .to_string();
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
    .commit("test-operator")
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
async fn denials_reach_paths_grants_cannot() -> anyhow::Result<()> {
    // CHAOS_HOME inside a trusted directory, like a grant on `~`.
    let outer = tempfile::tempdir()?;
    let home_dir = outer.path().join("home");
    std::fs::create_dir(&home_dir)?;
    crate::test_support::bootstrap_sqlite(&home_dir)?;
    let outer_key = key(&outer);
    crate::config::set_project_trust_level(&home_dir, outer.path(), TrustLevel::Trusted)?;
    // Granting it again through the operator is refused ...
    assert!(
        prepare_set(
            &home_dir,
            outer.path(),
            Decision::Trusted,
            Decision::Trusted
        )
        .await
        .is_err()
    );
    // ... but the existing grant can be revoked.
    prepare_set(
        &home_dir,
        outer.path(),
        Decision::Untrusted,
        Decision::Trusted,
    )
    .await?
    .commit("test-operator")
    .await?;
    assert_eq!(stored(&home_dir, &outer_key).await, Decision::Untrusted);

    // A grant on a directory that has since been deleted can be revoked by its
    // stored key; a missing path with nothing stored cannot.
    let gone = tempfile::tempdir()?;
    let gone_key = key(&gone);
    crate::config::set_project_trust_level(&home_dir, gone.path(), TrustLevel::Trusted)?;
    let gone_path = gone.path().to_path_buf();
    drop(gone);
    assert!(!gone_path.exists());
    prepare_set(
        &home_dir,
        Path::new(&gone_key),
        Decision::Untrusted,
        Decision::Trusted,
    )
    .await?
    .commit("test-operator")
    .await?;
    assert_eq!(stored(&home_dir, &gone_key).await, Decision::Untrusted);
    assert!(
        prepare_set(
            &home_dir,
            &outer.path().join("never-existed"),
            Decision::Untrusted,
            Decision::None
        )
        .await
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn set_resolves_to_the_key_the_gates_read() -> anyhow::Result<()> {
    let home = sqlite_home().await;
    let repo = tempfile::tempdir()?;
    std::fs::create_dir(repo.path().join(".git"))?;
    let sub = repo.path().join("crates").join("one");
    std::fs::create_dir_all(&sub)?;
    let err = prepare_set(home.path(), &sub, Decision::Trusted, Decision::None)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("decided at its project root"), "{err}");
    assert!(err.contains(&key(&repo)), "{err}");
    // The root itself is accepted, and is the key hooks read.
    let prepared = prepare_set(home.path(), repo.path(), Decision::Trusted, Decision::None).await?;
    assert_eq!(prepared.changes[0].project_path, key(&repo));
    assert_eq!(
        trust_key(&sub)?.to_string_lossy(),
        crate::hooks::project_root(&sub)?.to_string_lossy()
    );

    // Apply shares the guard: the same subdirectory grant is held, naming the
    // root, and nothing is written or retargeted.
    let sub_key = normalize_project_path(&sub)?.to_string_lossy().to_string();
    let manifest = Manifest {
        format: MANIFEST_FORMAT.to_string(),
        installation_id: crate::user_settings::installation_id(home.path())?,
        entries: vec![ManifestEntry {
            project_path: sub_key.clone(),
            decision: Decision::Trusted,
            expect: None,
        }],
    };
    let json = serde_json::to_string(&manifest)?;
    let receipt = prepare_apply(home.path(), &json, &manifest.digest()?)
        .await?
        .commit("test-operator")
        .await?;
    assert_eq!(receipt.held.len(), 1, "{receipt:?}");
    assert_eq!(receipt.held[0].project_path, sub_key);
    assert!(
        receipt.held[0].reason.contains(&key(&repo)),
        "{}",
        receipt.held[0].reason
    );
    assert!(receipt.changes.is_empty());
    assert_eq!(stored(home.path(), &sub_key).await, Decision::None);
    assert_eq!(stored(home.path(), &key(&repo)).await, Decision::None);
    Ok(())
}

/// A grant stored on a directory that later moved under a Git root still sits
/// on an exact key the config loader reads. It must stay revocable in place;
/// a denial below a root with no stored row is still refused as misleading.
#[tokio::test]
async fn stored_key_below_a_new_root_stays_revocable() -> anyhow::Result<()> {
    let home = sqlite_home().await;
    let parent = tempfile::tempdir()?;
    let old = parent.path().join("old");
    let other = parent.path().join("other");
    std::fs::create_dir_all(&old)?;
    std::fs::create_dir_all(&other)?;
    prepare_set(home.path(), &old, Decision::Trusted, Decision::None)
        .await?
        .commit("test-operator")
        .await?;
    std::fs::create_dir(parent.path().join(".git"))?;
    let old_key = normalize_project_path(&old)?.to_string_lossy().to_string();
    assert_ne!(trust_key(&old)?.to_string_lossy(), old_key);

    let receipt = prepare_set(home.path(), &old, Decision::Untrusted, Decision::Trusted)
        .await?
        .commit("test-operator")
        .await?;
    assert_eq!(receipt.changes[0].project_path, old_key);
    assert_eq!(stored(home.path(), &old_key).await, Decision::Untrusted);
    let root = normalize_project_path(parent.path())?
        .to_string_lossy()
        .to_string();
    assert_eq!(stored(home.path(), &root).await, Decision::None);

    let err = prepare_set(home.path(), &other, Decision::Untrusted, Decision::None)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("decided at its project root"), "{err}");
    Ok(())
}

#[tokio::test]
async fn manifest_grants_that_fail_the_guards_are_held_not_written() -> anyhow::Result<()> {
    let home = sqlite_home().await;
    let present = tempfile::tempdir()?;
    let missing = present.path().join("missing");
    let manifest = Manifest {
        format: MANIFEST_FORMAT.to_string(),
        installation_id: crate::user_settings::installation_id(home.path())?,
        entries: vec![
            ManifestEntry {
                project_path: key(&present),
                decision: Decision::Trusted,
                expect: None,
            },
            ManifestEntry {
                project_path: missing.to_string_lossy().to_string(),
                decision: Decision::Trusted,
                expect: None,
            },
            ManifestEntry {
                project_path: missing.join("denied").to_string_lossy().to_string(),
                decision: Decision::Untrusted,
                expect: None,
            },
        ],
    };
    let json = serde_json::to_string(&manifest)?;
    let prepared = prepare_apply(home.path(), &json, &manifest.digest()?).await?;
    assert_eq!(prepared.held.len(), 1);
    assert_eq!(prepared.held[0].project_path, missing.to_string_lossy());
    let receipt = prepared.commit("test-operator").await?;
    assert_eq!(receipt.held.len(), 1);
    assert_eq!(receipt.changes.len(), 2);
    assert_eq!(stored(home.path(), &key(&present)).await, Decision::Trusted);
    assert_eq!(
        stored(home.path(), &missing.to_string_lossy()).await,
        Decision::None
    );
    // A denial on a path that does not exist is carried.
    assert_eq!(
        stored(home.path(), &missing.join("denied").to_string_lossy()).await,
        Decision::Untrusted
    );
    Ok(())
}

#[tokio::test]
async fn every_change_is_audited_with_actor_and_installation() -> anyhow::Result<()> {
    let home = sqlite_home().await;
    let project = tempfile::tempdir()?;
    let installation = crate::user_settings::installation_id(home.path())?;
    prepare_set(
        home.path(),
        project.path(),
        Decision::Trusted,
        Decision::None,
    )
    .await?
    .commit("operator:alice")
    .await?;
    // A no-op recheck writes no event.
    prepare_set(
        home.path(),
        project.path(),
        Decision::Trusted,
        Decision::Trusted,
    )
    .await?
    .commit("operator:alice")
    .await?;
    let db = crate::user_settings::open(home.path()).await?;
    let RuntimeDbHandle::Sqlite(runtime) = db else {
        anyhow::bail!("expected sqlite");
    };
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT event, subject FROM configuration_events WHERE event LIKE 'project_trust_%'",
    )
    .fetch_all(runtime.pool())
    .await?;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, "project_trust_trusted");
    let subject: serde_json::Value = serde_json::from_str(&rows[0].1)?;
    assert_eq!(subject["actor"], "operator:alice");
    assert_eq!(subject["installation_id"], installation.as_str());
    assert_eq!(subject["before"], "none");
    assert_eq!(subject["project_path"], key(&project).as_str());
    Ok(())
}

#[tokio::test]
async fn reading_trust_does_not_create_an_installation_identity() -> anyhow::Result<()> {
    let home = sqlite_home().await;
    let identity = home.path().join("installation-id");
    let _ = std::fs::remove_file(&identity);
    let listing = list(home.path()).await?;
    assert_eq!(listing.installation_id, None);
    assert!(export(home.path()).await.is_err());
    assert!(!identity.exists());
    // A malformed identity is an error, never "no identity".
    std::fs::write(&identity, "not-a-uuid\n")?;
    assert!(list(home.path()).await.is_err());
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
        .set_project_trust(Path::new(&key(&a)), TrustLevel::Untrusted, None)
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
        .commit("test-operator")
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
        .commit("test-operator")
        .await?;
    let manifest = export(home.path()).await?;
    let json = serde_json::to_string(&manifest)?;
    let receipt = prepare_apply(home.path(), &json, &manifest.digest()?)
        .await?
        .commit("test-operator")
        .await?;
    assert!(receipt.changes.is_empty());
    assert_eq!(receipt.unchanged, vec![key(&a)]);
    Ok(())
}

/// Same installation moving from SQLite to PostgreSQL: granted AND denied decisions
/// arrive; nothing else does.
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
    .commit("test-operator")
    .await?;
    prepare_set(
        source.path(),
        denied.path(),
        Decision::Untrusted,
        Decision::None,
    )
    .await?
    .commit("test-operator")
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
        .commit("test-operator")
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
        .commit("test-operator")
        .await?;
    assert!(receipt.changes.is_empty());
    prepare_set(
        destination.path(),
        denied.path(),
        Decision::Trusted,
        Decision::Untrusted,
    )
    .await?
    .commit("test-operator")
    .await?;
    assert!(
        prepare_apply(destination.path(), &json, &manifest.digest()?)
            .await
            .is_err()
    );
    // Clean the shared database (the admin interface cannot delete decisions).
    sqlx::query("DELETE FROM project_trust WHERE project_path = ANY($1)")
        .bind(vec![key(&trusted), key(&denied)])
        .execute(&sqlx::PgPool::connect(&url).await?)
        .await?;
    Ok(())
}

/// Onboarding races a writer that inserts a denial for the same, previously
/// absent, project. Onboarding must wait for that insert and audit the denial
/// it replaced as `before`, not `none` (what SELECT .. FOR UPDATE on a missing
/// row followed by an upsert would record).
async fn postgres_onboarding_audits_the_decision_it_replaced() -> anyhow::Result<()> {
    let Some(url) = postgres_url() else {
        tracing::warn!("skipping postgres onboarding race test; TEST_DATABASE_URL not set");
        return Ok(());
    };
    let home = postgres_home(&url, None);
    let project = tempfile::tempdir()?;
    let path = key(&project);
    let db = crate::user_settings::open(home.path()).await?;

    let pool = sqlx::PgPool::connect(&url).await?;
    let mut other = pool.begin().await?;
    let writer: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *other)
        .await?;
    sqlx::query(
        "INSERT INTO project_trust (project_path, trust_level, created_at, updated_at) \
         VALUES ($1, 'untrusted', 0, 0)",
    )
    .bind(&path)
    .execute(&mut *other)
    .await?;
    let onboarding = {
        let db = db.clone();
        let path = path.clone();
        tokio::spawn(async move {
            db.set_project_trust(Path::new(&path), TrustLevel::Trusted, Some("inst-test"))
                .await
        })
    };

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let blocked = loop {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
             WHERE $1 = ANY(pg_blocking_pids(pid)) AND query LIKE '%project_trust%'",
        )
        .bind(writer)
        .fetch_one(&pool)
        .await?;
        if waiting > 0 {
            break true;
        }
        if onboarding.is_finished() || tokio::time::Instant::now() >= deadline {
            break false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    let cleanup = || async {
        sqlx::query("DELETE FROM project_trust WHERE project_path = $1")
            .bind(&path)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM configuration_events WHERE subject LIKE $1")
            .bind(format!("%{path}%"))
            .execute(&pool)
            .await
    };
    if !blocked {
        other.rollback().await?;
        let _ = onboarding.await;
        cleanup().await?;
        panic!("onboarding never waited on the concurrent insert");
    }
    other.commit().await?;
    onboarding.await??;

    let subjects: Vec<String> = sqlx::query_scalar(
        "SELECT subject FROM configuration_events \
         WHERE event = 'project_trust_trusted' AND subject LIKE $1",
    )
    .bind(format!("%{path}%"))
    .fetch_all(&pool)
    .await?;
    cleanup().await?;
    assert_eq!(subjects.len(), 1, "{subjects:?}");
    let subject: serde_json::Value = serde_json::from_str(&subjects[0])?;
    assert_eq!(subject["before"], "untrusted");
    assert_eq!(subject["actor"], "onboarding");
    assert_eq!(subject["installation_id"], "inst-test");
    Ok(())
}

/// chaos_vfs mounts storage once per process, keyed by URL, so every Postgres
/// home here shares the pool of whichever runtime mounted it first. Run the
/// Postgres scenarios in one runtime so that pool outlives all of them.
#[tokio::test]
async fn postgres_trust_scenarios() -> anyhow::Result<()> {
    sqlite_to_postgres_migration_preserves_granted_and_denied().await?;
    unchanged_entries_are_rechecked_at_commit_postgres().await?;
    postgres_unchanged_entry_is_locked_against_concurrent_change().await?;
    postgres_onboarding_audits_the_decision_it_replaced().await
}
