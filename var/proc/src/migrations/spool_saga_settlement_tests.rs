use std::borrow::Cow;

use serde_json::{Value, json};
use sqlx::migrate::{Migration, Migrator};
use sqlx::{Connection, Row};

use super::{POSTGRES_STATE_MIGRATOR, STATE_MIGRATOR};

const SQLITE_VERSION: i64 = 41;
const POSTGRES_VERSION: i64 = 40;

const INSERT: &str = "INSERT INTO spool_jobs
     (manifest_id, backend, batch_id, status, request_count, payload_json,
      created_at, updated_at, saga_json)
     VALUES ($1, 'mock', $2, $3, 1, '[]', 10, 10, $4)";
const SELECT: &str =
    "SELECT manifest_id, status, error, saga_json FROM spool_jobs ORDER BY manifest_id";

fn settled() -> Value {
    json!({"direction": "forward", "step": 0, "attempt": 1, "outcome": "success"})
}

fn fixtures() -> Vec<(&'static str, Option<&'static str>, &'static str, Value)> {
    vec![
        (
            "a-forward",
            Some("batch-a"),
            "InProgress",
            json!({"direction": "forward", "step": 2, "attempt": 3, "poll_cron_id": "cron-a"}),
        ),
        (
            "b-first-step",
            None,
            "Queued",
            json!({"direction": "forward", "step": 0, "attempt": 0}),
        ),
        (
            "c-compensating",
            Some("batch-c"),
            "InProgress",
            json!({"direction": "compensating", "step": 1, "attempt": 4, "error": "boom"}),
        ),
        (
            "d-compensating-unsubmitted",
            None,
            "Queued",
            json!({"direction": "compensating", "step": 0, "attempt": 2}),
        ),
        (
            "e-settled",
            Some("batch-e"),
            "InProgress",
            json!({"direction": "forward", "step": 1, "attempt": 1, "settled": settled()}),
        ),
    ]
}

fn prior_migrations(migrator: &'static Migrator, version: i64) -> &'static [Migration] {
    let count = migrator
        .migrations
        .partition_point(|step| step.version < version);
    &migrator.migrations[..count]
}

struct Migrated {
    manifest_id: String,
    status: String,
    error: Option<String>,
    saga: Option<Value>,
}

fn check_rows(rows: Vec<Migrated>) {
    let [forward, first, compensating, unsubmitted, already] = rows.as_slice() else {
        panic!("expected five spool rows");
    };
    assert_eq!(forward.manifest_id, "a-forward");
    assert_eq!(forward.status, "InProgress");
    assert_eq!(
        forward.saga,
        Some(json!({
            "direction": "forward", "step": 2, "attempt": 3, "poll_cron_id": "cron-a",
            "settled": {"direction": "forward", "step": 1, "attempt": 3, "outcome": "success"}
        }))
    );

    assert_eq!(first.status, "Queued");
    assert_eq!(first.saga, Some(fixtures()[1].3.clone()));

    assert_eq!(compensating.status, "Failed");
    assert_eq!(compensating.saga, None);
    let error = compensating.error.as_deref().expect("failure reason");
    assert!(error.contains("cannot resume"), "{error}");
    assert!(error.contains("cancel batch batch-c by hand"), "{error}");

    assert_eq!(unsubmitted.status, "Failed");
    assert_eq!(unsubmitted.saga, None);
    let error = unsubmitted.error.as_deref().expect("failure reason");
    assert!(!error.contains("cancel batch"), "{error}");

    assert_eq!(already.status, "InProgress");
    assert_eq!(already.saga, Some(fixtures()[4].3.clone()));
}

fn migrated<R: Row>(row: &R) -> Migrated
where
    for<'r> String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> Option<String>: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    &'static str: sqlx::ColumnIndex<R>,
{
    Migrated {
        manifest_id: row.get("manifest_id"),
        status: row.get("status"),
        error: row.get("error"),
        saga: row
            .get::<Option<String>, _>("saga_json")
            .map(|saga| serde_json::from_str(&saga).expect("saga json")),
    }
}

#[tokio::test]
async fn sqlite_spool_sagas_gain_a_settlement_or_fail() -> anyhow::Result<()> {
    let mut connection = sqlx::SqliteConnection::connect("sqlite::memory:").await?;
    Migrator {
        migrations: Cow::Borrowed(prior_migrations(&STATE_MIGRATOR, SQLITE_VERSION)),
        ..Migrator::DEFAULT
    }
    .run(&mut connection)
    .await?;
    for (manifest_id, batch_id, status, saga) in fixtures() {
        sqlx::query(INSERT)
            .bind(manifest_id)
            .bind(batch_id)
            .bind(status)
            .bind(saga.to_string())
            .execute(&mut connection)
            .await?;
    }
    STATE_MIGRATOR.run(&mut connection).await?;
    let rows = sqlx::query(SELECT).fetch_all(&mut connection).await?;
    check_rows(rows.iter().map(migrated).collect());
    connection.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; uses a transaction-local schema and rolls it back"]
async fn postgres_spool_sagas_gain_a_settlement_or_fail() -> anyhow::Result<()> {
    let url = std::env::var("TEST_DATABASE_URL")?;
    let mut connection = sqlx::PgConnection::connect(&url).await?;
    let mut tx = connection.begin().await?;
    let schema = format!("spool_saga_test_{}", uuid::Uuid::new_v4().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE SCHEMA {schema}; SET LOCAL search_path = {schema};"
    )))
    .execute(&mut *tx)
    .await?;
    for step in prior_migrations(&POSTGRES_STATE_MIGRATOR, POSTGRES_VERSION) {
        sqlx::raw_sql(step.sql.as_str()).execute(&mut *tx).await?;
    }
    for (manifest_id, batch_id, status, saga) in fixtures() {
        sqlx::query(INSERT)
            .bind(manifest_id)
            .bind(batch_id)
            .bind(status)
            .bind(saga.to_string())
            .execute(&mut *tx)
            .await?;
    }
    let upgrade = POSTGRES_STATE_MIGRATOR
        .migrations
        .iter()
        .find(|step| step.version == POSTGRES_VERSION)
        .expect("upgrade migration");
    sqlx::raw_sql(upgrade.sql.as_str())
        .execute(&mut *tx)
        .await?;
    let rows = sqlx::query(SELECT).fetch_all(&mut *tx).await?;
    check_rows(rows.iter().map(migrated).collect());
    tx.rollback().await?;
    connection.close().await?;
    Ok(())
}
