use std::borrow::Cow;
use std::num::NonZeroU32;
use std::time::Duration;

use outbox_machines::{Claimed, Outbox, State, Stored};
use pretty_assertions::assert_eq;
use sqlx::Connection;
use sqlx::migrate::{Migration, Migrator};

use super::{POSTGRES_STATE_MIGRATOR, STATE_MIGRATOR};

const SQLITE_VERSION: i64 = 42;
const POSTGRES_VERSION: i64 = 41;

const SCOPE: &str = "INSERT INTO execution_scopes (scope) VALUES ('legacy')";
const SESSION: &str =
    "INSERT INTO execution_sessions (session_id, scope, state, owner_id, expires_at_ms)
     VALUES ('session', 'legacy', 'idle', '', 0)";
const INSERT: &str = "INSERT INTO execution_outbox
     (event_id, event_key, scope, session_id, kind, created_at_ms, body_json,
      attempts, next_attempt_at_ms, owner_id, lease_token, last_issued, expires_at_ms,
      delivered_at_ms)
     VALUES ($1, $1, 'legacy', 'session', 'turn.terminal', 1000, '{}', $2, $3, 'worker',
             $4, $5, $6, $7)";
const SELECT: &str = "SELECT event_id, delivery_state, attempts, lease_token, last_issued,
     expires_at_ms, next_attempt_at_ms, failed_after_ms, observed_at_ms, dead_reason
     FROM execution_outbox ORDER BY event_id";

type Legacy = (&'static str, i64, i64, Option<i64>, i64, i64, Option<i64>);
type Migrated = (
    String,
    String,
    i64,
    Option<i64>,
    i64,
    i64,
    i64,
    Option<i64>,
    i64,
    Option<String>,
);

fn fixtures() -> Vec<Legacy> {
    vec![
        ("a-fresh", 0, 1000, None, 0, 0, None),
        ("b-claimed", 2, 1000, Some(2), 2, 50_000, None),
        ("c-retrying", 3, 9000, None, 3, 0, None),
        ("d-delivered", 1, 1000, Some(1), 1, 0, Some(2000)),
        ("e-exhausted-long-ago", 40, 5000, None, 40, 0, None),
        ("f-exhausted", 32, 5000, None, 32, 0, None),
        ("g-pre-fencing-holder", 5, 1000, Some(1), 1, 60_000, None),
    ]
}

fn restorable(row: &Migrated) -> bool {
    let millis = |value: i64| u64::try_from(value).ok().map(Duration::from_millis);
    let state = match row.1.as_str() {
        "pending" => State::Pending,
        "claimed" => State::Claimed,
        "confirmed" => State::Confirmed,
        "dead" => State::Dead,
        _ => return false,
    };
    let latest = match row.3 {
        Some(token) => match (u64::try_from(token).ok(), millis(row.5)) {
            (Some(attempt), Some(expires)) => Claimed::new(0, attempt, expires),
            _ => None,
        }
        .map(Some),
        None => Some(None),
    };
    let failed = match row.7 {
        Some(delay) => millis(delay).map(Some),
        None => Some(None),
    };
    let (Some(latest), Some(failed), Ok(attempts), Some(available)) =
        (latest, failed, u32::try_from(row.2), millis(row.6))
    else {
        return false;
    };
    let (Some(stored), Ok(issued), Some(observed)) = (
        Stored::new(0, state, attempts, latest, available, failed),
        u64::try_from(row.4),
        millis(row.8),
    ) else {
        return false;
    };
    Outbox::<1>::restored(NonZeroU32::new(32).unwrap(), 0, issued, observed, stored).is_some()
}

fn check(rows: &[Migrated]) {
    let summary: Vec<_> = rows
        .iter()
        .map(|row| {
            (
                row.0.as_str(),
                row.1.as_str(),
                row.2,
                row.3,
                row.4,
                row.9.as_deref(),
                restorable(row),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("a-fresh", "pending", 0, None, 0, None, true),
            ("b-claimed", "claimed", 2, Some(2), 2, None, true),
            ("c-retrying", "pending", 3, Some(3), 3, None, true),
            ("d-delivered", "confirmed", 1, Some(1), 1, None, true),
            (
                "e-exhausted-long-ago",
                "dead",
                40,
                Some(40),
                40,
                Some("attempts exhausted"),
                false
            ),
            (
                "f-exhausted",
                "dead",
                32,
                Some(32),
                32,
                Some("attempts exhausted"),
                true
            ),
            ("g-pre-fencing-holder", "pending", 5, Some(5), 5, None, true),
        ]
    );
    let held = &rows[1];
    assert_eq!((held.5, held.6), (50_000, 1000), "the live lease survives");
    let fenced = &rows[6];
    assert_eq!(
        (fenced.5, fenced.6),
        (60_000, 60_000),
        "a pre-fencing holder is fenced, the row is due when its lease would have ended"
    );
}

fn prior_migrations(migrator: &'static Migrator, version: i64) -> &'static [Migration] {
    let count = migrator
        .migrations
        .partition_point(|step| step.version < version);
    &migrator.migrations[..count]
}

#[tokio::test]
async fn sqlite_legacy_deliveries_load_as_outbox_records() -> anyhow::Result<()> {
    let mut connection = sqlx::SqliteConnection::connect("sqlite::memory:").await?;
    Migrator {
        migrations: Cow::Borrowed(prior_migrations(&STATE_MIGRATOR, SQLITE_VERSION)),
        ..Migrator::DEFAULT
    }
    .run(&mut connection)
    .await?;
    sqlx::query(SCOPE).execute(&mut connection).await?;
    sqlx::query(SESSION).execute(&mut connection).await?;
    for (id, attempts, next, token, issued, expires, delivered) in fixtures() {
        sqlx::query(INSERT)
            .bind(id)
            .bind(attempts)
            .bind(next)
            .bind(token)
            .bind(issued)
            .bind(expires)
            .bind(delivered)
            .execute(&mut connection)
            .await?;
    }
    STATE_MIGRATOR.run(&mut connection).await?;
    let rows: Vec<Migrated> = sqlx::query_as(SELECT).fetch_all(&mut connection).await?;
    check(&rows);
    connection.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; uses a transaction-local schema and rolls it back"]
async fn postgres_legacy_deliveries_load_as_outbox_records() -> anyhow::Result<()> {
    let url = std::env::var("TEST_DATABASE_URL")?;
    let mut connection = sqlx::PgConnection::connect(&url).await?;
    let mut tx = connection.begin().await?;
    let schema = format!("delivery_state_test_{}", uuid::Uuid::new_v4().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE SCHEMA {schema}; SET LOCAL search_path = {schema};"
    )))
    .execute(&mut *tx)
    .await?;
    for step in prior_migrations(&POSTGRES_STATE_MIGRATOR, POSTGRES_VERSION) {
        sqlx::raw_sql(step.sql.as_str()).execute(&mut *tx).await?;
    }
    sqlx::query(SCOPE).execute(&mut *tx).await?;
    sqlx::query(SESSION).execute(&mut *tx).await?;
    for (id, attempts, next, token, issued, expires, delivered) in fixtures() {
        sqlx::query(INSERT)
            .bind(id)
            .bind(attempts)
            .bind(next)
            .bind(token)
            .bind(issued)
            .bind(expires)
            .bind(delivered)
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
    let rows: Vec<Migrated> = sqlx::query_as(SELECT).fetch_all(&mut *tx).await?;
    check(&rows);
    tx.rollback().await?;
    connection.close().await?;
    Ok(())
}
