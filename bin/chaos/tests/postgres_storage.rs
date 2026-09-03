use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use chaos_kern::runtime_db::{LogEntry, open_or_create_runtime_db_with_config};
use tokio::process::Command;
use tokio::time::timeout;

#[test]
fn console_logging_uses_the_mounted_runtime_database() {
    let console_startup = include_str!("../../console/src/lib.rs");
    let log_panel = include_str!("../../console/src/app/log_panel.rs");

    for (path, source) in [
        ("bin/console/src/lib.rs", console_startup),
        ("bin/console/src/app/log_panel.rs", log_panel),
    ] {
        assert!(
            !source.contains("StateRuntime::init"),
            "{path} must not bypass the configured storage backend by opening SQLite directly"
        );
        assert!(
            source.contains("get_runtime_db"),
            "{path} must reuse the mounted runtime database"
        );
    }
}

#[tokio::test]
async fn unavailable_postgres_fails_without_creating_sqlite() -> anyhow::Result<()> {
    let chaos_home = tempfile::tempdir()?;
    std::fs::write(
        chaos_home.path().join("config.toml"),
        r#"
storage_url = "postgres://chaos:chaos@127.0.0.1:1/chaos?connect_timeout=1"
"#,
    )?;

    let chaos_cli = chaos_which::cargo_bin("chaos")?;
    let output = timeout(
        Duration::from_secs(15),
        Command::new(chaos_cli)
            .arg("-c")
            .arg("analytics.enabled=false")
            .env("CHAOS_HOME", chaos_home.path())
            .env_remove("CHAOS_SQLITE_HOME")
            .env_remove("CHAOS_JOURNALD_SOCKET")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for PostgreSQL startup failure"))??;

    assert!(
        !output.status.success(),
        "an unavailable configured PostgreSQL backend must fail startup"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    for relative in [
        "chaos.sqlite",
        "chaos.sqlite-wal",
        "chaos.sqlite-shm",
        "run/journald.sock",
        "run/journald.lock",
    ] {
        assert!(
            !chaos_home.path().join(relative).exists(),
            "PostgreSQL startup failure unexpectedly created {relative}; stderr: {stderr}"
        );
    }

    Ok(())
}

#[tokio::test]
#[ignore = "requires CHAOS_TEST_POSTGRES_URL"]
async fn live_postgres_log_round_trip_creates_no_sqlite() -> anyhow::Result<()> {
    let storage_url = std::env::var("CHAOS_TEST_POSTGRES_URL")
        .map_err(|_| anyhow::anyhow!("CHAOS_TEST_POSTGRES_URL must be set"))?;
    let chaos_home = tempfile::tempdir()?;
    let runtime = open_or_create_runtime_db_with_config(
        Some(storage_url.as_str()),
        chaos_home.path(),
        "test-provider",
    )
    .await?;
    let marker = format!(
        "chaos-postgres-log-smoke-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );

    runtime
        .insert_logs(&[LogEntry {
            ts: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64,
            ts_nanos: 0,
            level: "INFO".to_string(),
            target: "postgres-storage-test".to_string(),
            message: Some(marker.clone()),
            process_id: None,
            process_uuid: None,
            module_path: Some("postgres_storage".to_string()),
            file: Some(file!().to_string()),
            line: Some(line!().into()),
        }])
        .await?;

    let pool = sqlx::PgPool::connect(storage_url.as_str()).await?;
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM logs WHERE message = $1")
        .bind(marker.as_str())
        .fetch_one(&pool)
        .await?;
    sqlx::query("DELETE FROM logs WHERE message = $1")
        .bind(marker)
        .execute(&pool)
        .await?;
    pool.close().await;

    assert_eq!(count, 1, "the log row was not persisted in PostgreSQL");
    for relative in ["chaos.sqlite", "chaos.sqlite-wal", "chaos.sqlite-shm"] {
        assert!(
            !chaos_home.path().join(relative).exists(),
            "PostgreSQL log round-trip unexpectedly created {relative}"
        );
    }

    Ok(())
}
