use super::*;
use crate::rollout::list::parse_cursor;
use jiff::Timestamp;
use pretty_assertions::assert_eq;

#[test]
fn cursor_to_anchor_normalizes_timestamp_format() {
    let uuid = Uuid::new_v4();
    let ts_str = "2026-01-27T12-34-56";
    let token = format!("{ts_str}|{uuid}");
    let cursor = parse_cursor(token.as_str()).expect("cursor should parse");
    let anchor = cursor_to_anchor(Some(&cursor)).expect("anchor should parse");

    let expected_ts: Timestamp = "2026-01-27T12:34:56Z".parse().expect("ts should parse");

    assert_eq!(anchor.id, uuid);
    assert_eq!(anchor.ts, expected_ts);
}

#[tokio::test]
async fn open_runtime_db_uses_storage_url() {
    let chaos_home = tempfile::tempdir().expect("create chaos home");
    let storage_dir = tempfile::tempdir().expect("create storage dir");
    let db_path = storage_dir.path().join("configured.sqlite");
    let storage_url = format!("sqlite://{}", db_path.display());

    let runtime = open_or_create_runtime_db(&storage_url, chaos_home.path(), "test-provider")
        .await
        .expect("open runtime db from bootstrap storage_url");

    std::assert_matches!(
        runtime,
        RuntimeDbHandle::Sqlite(_),
        "sqlite storage_url should create a sqlite runtime"
    );
    assert!(
        tokio::fs::try_exists(&db_path)
            .await
            .expect("stat configured db"),
        "configured sqlite URL should create the target db"
    );
    assert!(
        !tokio::fs::try_exists(&chaos_proc::runtime_db_path(chaos_home.path()))
            .await
            .expect("stat fallback db"),
        "storage_url decides the backend; chaos_home only keys local state"
    );
}

#[tokio::test]
async fn failed_mount_does_not_hide_the_next_mount_error_behind_runtime_breaker() {
    let chaos_home = tempfile::tempdir().expect("create chaos home");
    let missing_parent = chaos_home.path().join("missing");
    let storage_url = format!(
        "sqlite://{}",
        missing_parent.join("runtime.sqlite").display()
    );

    let first = mount_vfs_for(&storage_url, chaos_home.path())
        .await
        .expect_err("opening SQLite below a missing parent should fail");
    assert!(
        first.to_string().contains("cannot reach runtime db"),
        "first mount should expose the storage error: {first}"
    );

    let second = mount_vfs_for(&storage_url, chaos_home.path())
        .await
        .expect_err("a repeated mount should still report the storage failure");
    assert!(
        second.to_string().contains("cannot reach runtime db"),
        "repeated mount should not be replaced by a circuit-open error: {second}"
    );
    assert!(
        !second.to_string().contains("circuit"),
        "mount errors must remain actionable: {second}"
    );
}

#[tokio::test]
async fn a_vanished_sqlite_db_is_reported_instead_of_recreated_empty() -> anyhow::Result<()> {
    let chaos_home = tempfile::tempdir()?;
    let data = chaos_home.path().join("data");
    std::fs::create_dir_all(&data)?;
    let db_path = data.join("runtime.sqlite");
    let record = chaos_home.path().join(RUNTIME_DB_LOCATION_FILE);
    let config = mount_config_for(
        &format!("sqlite://{}", db_path.display()),
        chaos_home.path(),
    )?;

    let vfs = open_vfs(config.clone(), chaos_home.path()).await?;
    if let Some(pool) = vfs.sqlite_pool() {
        pool.close().await;
    }
    assert_eq!(
        std::fs::read_to_string(&record)?,
        db_path.display().to_string()
    );

    std::fs::remove_dir_all(&data)?;
    std::fs::create_dir_all(&data)?;
    let err = open_vfs(config.clone(), chaos_home.path())
        .await
        .expect_err("a database this home opened before must not come back empty");
    let message = explain_storage_failure(chaos_home.path(), err).to_string();
    assert!(
        message.contains(&format!("runtime db {} is gone", db_path.display())),
        "{message}"
    );
    assert!(message.contains("if the database moved"), "{message}");
    assert!(
        message.contains(&format!("remove {}", record.display())),
        "{message}"
    );
    assert!(!db_path.exists());

    forget_runtime_db_location(chaos_home.path())?;
    open_vfs(config, chaos_home.path()).await?;
    assert!(db_path.exists());
    Ok(())
}

#[test]
fn choosing_storage_forgets_the_recorded_sqlite_location() -> anyhow::Result<()> {
    let chaos_home = tempfile::tempdir()?;
    let record = chaos_home.path().join(RUNTIME_DB_LOCATION_FILE);
    std::fs::write(&record, "/gone/chaos.sqlite")?;

    crate::user_settings::set_bootstrap(
        chaos_home.path(),
        "egress_url",
        "https://gateway.example",
    )?;
    assert!(record.exists());
    crate::user_settings::set_bootstrap(
        chaos_home.path(),
        "storage_url",
        "sqlite:///gone/chaos.sqlite",
    )?;
    assert!(!record.exists());
    Ok(())
}

#[test]
fn home_local_sqlite_urls_mount_under_the_home_key() {
    let home = tempfile::tempdir().expect("create chaos home");
    let url = format!("sqlite://{}", home.path().join("chaos.sqlite").display());
    assert_eq!(
        mount_config_for(&url, home.path()).expect("mount config"),
        chaos_vfs::MountConfig::sqlite_home(home.path())
    );
    let elsewhere = format!("sqlite://{}", home.path().join("other.sqlite").display());
    assert_eq!(
        mount_config_for(&elsewhere, home.path()).expect("mount config"),
        chaos_vfs::MountConfig::sqlite_url(&elsewhere)
    );
    let with_options = format!("{url}?mode=ro");
    assert_eq!(
        mount_config_for(&with_options, home.path()).expect("mount config"),
        chaos_vfs::MountConfig::sqlite_url(&with_options)
    );
}

#[tokio::test]
async fn runtime_storage_breaker_is_scoped_to_chaos_home() {
    let failing = tempfile::tempdir().expect("create failing home");
    let healthy = tempfile::tempdir().expect("create healthy home");

    assert!(Arc::ptr_eq(
        &runtime_storage_breaker(failing.path()),
        &runtime_storage_breaker(failing.path()),
    ));

    let failed = runtime_storage_breaker(failing.path())
        .call(|| async { Err::<(), _>("no such table: processes") })
        .await;
    std::assert_matches!(failed, Err(BreakerError::Operation(_)));

    let rejected = runtime_storage_breaker(failing.path())
        .call(|| async { Ok::<(), &str>(()) })
        .await;
    std::assert_matches!(rejected, Err(BreakerError::Open));

    let isolated = runtime_storage_breaker(healthy.path())
        .call(|| async { Ok::<_, &str>("served") })
        .await;
    std::assert_matches!(isolated, Ok("served"));
}
