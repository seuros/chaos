use std::borrow::Cow;
use std::ops::AsyncFnMut;

use chaos_ipc::protocol::TurnContextItem;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::migrate::{Migration, Migrator};
use sqlx::types::Json;
use sqlx::{ColumnIndex, Connection, Database, Decode, Encode, Executor, IntoArguments, Row, Type};

use super::{POSTGRES_STATE_MIGRATOR, STATE_MIGRATOR};

const PROCESS_ID: &str = "01a07d43-796f-7881-a045-fdfe8dbdb79a";
const SQLITE_UPGRADE: &str = include_str!("../../db/migrate/sqlite/0016_turn_model_provider.sql");
const POSTGRES_UPGRADE: &str =
    include_str!("../../db/migrate/postgres/0016_turn_model_provider.sql");

fn prior_migrations(migrator: &'static Migrator) -> &'static [Migration] {
    let count = migrator
        .migrations
        .partition_point(|step| step.version < 16);
    &migrator.migrations[..count]
}

fn turn() -> Value {
    json!({
        "type": "turn_context",
        "payload": {
            "cwd": "/tmp",
            "approval_policy": "headless",
            "vfs_policy": { "kind": "unrestricted" },
            "socket_policy": "restricted",
            "model": "saved-model",
            "effort": "high",
            "summary": "auto"
        }
    })
}

fn fixtures() -> Vec<Value> {
    let mut explicit = turn();
    explicit["payload"]["model_provider"] = json!("turn-provider");
    let mut null = turn();
    null["payload"]["model_provider"] = Value::Null;
    vec![
        turn(),
        json!({"type": "session_meta", "payload": {"model_provider": "session-provider"}}),
        turn(),
        explicit,
        null,
        json!({"type": "response_item", "payload": {"type": "other"}}),
        json!({"type": "session_meta", "payload": {"model_provider": "later-provider"}}),
        turn(),
    ]
}

fn check_rows(rows: Vec<(i64, i64, Value)>, upgraded: bool) {
    let expected = [
        Some("process-provider"),
        None,
        Some("session-provider"),
        Some("turn-provider"),
        Some("session-provider"),
        None,
        None,
        Some("later-provider"),
    ];
    assert_eq!(rows.len(), expected.len());
    for (index, ((seq, timestamp, actual), (mut original, provider))) in rows
        .into_iter()
        .zip(fixtures().into_iter().zip(expected))
        .enumerate()
    {
        assert_eq!(seq, index as i64);
        assert_eq!(timestamp, 100 + seq);
        if upgraded && let Some(provider) = provider {
            original["payload"]["model_provider"] = json!(provider);
            let turn =
                TurnContextItem::deserialize(&actual["payload"]).expect("strict turn must load");
            assert_eq!(turn.model_provider, provider);
            assert_eq!(turn.model, "saved-model");
        }
        assert_eq!(actual, original, "only the missing provider may change");
    }
}

/// The two backends exercise exactly the same fixtures, failure recovery,
/// idempotence, metadata preservation, and append-only checks.
async fn exercise_upgrade<DB, F>(
    connection: &mut DB::Connection,
    upgrade_sql: &'static str,
    mut apply_upgrade: F,
) -> anyhow::Result<()>
where
    DB: Database,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
    DB::Arguments: IntoArguments<DB>,
    for<'q> &'q str: Encode<'q, DB> + Type<DB>,
    for<'q> i64: Encode<'q, DB> + Decode<'q, DB> + Type<DB>,
    for<'q> Json<&'q Value>: Encode<'q, DB> + Type<DB>,
    for<'r> Json<Value>: Decode<'r, DB> + Type<DB>,
    usize: ColumnIndex<DB::Row>,
    F: for<'c> AsyncFnMut(&'c mut DB::Connection) -> anyhow::Result<()>,
{
    // Numbered parameters and SQLx's Json codec work on both backends.
    sqlx::query::<DB>(
        "INSERT INTO processes (id, source, source_json, model_provider, cwd, created_at, updated_at)
         VALUES ($1, 'cli', '\"cli\"', 'process-provider', '/tmp', 10, 20)",
    )
    .bind(PROCESS_ID)
    .execute(&mut *connection)
    .await?;
    for (seq, item) in fixtures().iter().enumerate() {
        sqlx::query::<DB>(
            "INSERT INTO journal_entries (process_id, seq, recorded_at, item_type, payload_json)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(PROCESS_ID)
        .bind(seq as i64)
        .bind(100 + seq as i64)
        .bind(item["type"].as_str().expect("fixture type"))
        .bind(Json(item))
        .execute(&mut *connection)
        .await?;
    }

    for upgraded in [false, true] {
        if upgraded {
            apply_upgrade(&mut *connection).await?;
            apply_upgrade(&mut *connection).await?;
            // Also check the repair SQL itself is idempotent.
            sqlx::raw_sql("SAVEPOINT repeat_upgrade")
                .execute(&mut *connection)
                .await?;
            sqlx::raw_sql(upgrade_sql).execute(&mut *connection).await?;
            sqlx::raw_sql("RELEASE SAVEPOINT repeat_upgrade")
                .execute(&mut *connection)
                .await?;
        } else {
            // A failed upgrade must roll back the data repair AND trigger DDL.
            sqlx::raw_sql("SAVEPOINT failed_upgrade")
                .execute(&mut *connection)
                .await?;
            sqlx::raw_sql(upgrade_sql).execute(&mut *connection).await?;
            assert!(
                sqlx::raw_sql("SELECT * FROM nonexistent_upgrade_test_table")
                    .execute(&mut *connection)
                    .await
                    .is_err()
            );
            sqlx::raw_sql("ROLLBACK TO SAVEPOINT failed_upgrade; RELEASE SAVEPOINT failed_upgrade")
                .execute(&mut *connection)
                .await?;
        }

        let rows = sqlx::query::<DB>(
            "SELECT seq, recorded_at, payload_json FROM journal_entries ORDER BY seq",
        )
        .fetch_all(&mut *connection)
        .await?;
        check_rows(
            rows.into_iter()
                .map(|row| (row.get(0), row.get(1), row.get::<Json<Value>, _>(2).0))
                .collect(),
            upgraded,
        );
        let updated_at: i64 =
            sqlx::query_scalar::<DB, i64>("SELECT updated_at FROM processes WHERE id = $1")
                .bind(PROCESS_ID)
                .fetch_one(&mut *connection)
                .await?;
        assert_eq!(updated_at, 20);
        for statement in [
            "UPDATE journal_entries SET recorded_at = 999",
            "DELETE FROM journal_entries",
        ] {
            sqlx::raw_sql("SAVEPOINT guard_check")
                .execute(&mut *connection)
                .await?;
            let error = sqlx::query::<DB>(statement)
                .execute(&mut *connection)
                .await
                .err()
                .expect("append-only guard");
            assert!(error.to_string().contains("append-only"));
            sqlx::raw_sql("ROLLBACK TO SAVEPOINT guard_check; RELEASE SAVEPOINT guard_check")
                .execute(&mut *connection)
                .await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn sqlite_turn_provider_upgrade_preserves_history_and_guards() -> anyhow::Result<()> {
    let mut connection = sqlx::SqliteConnection::connect("sqlite::memory:").await?;
    let before = Migrator {
        migrations: Cow::Borrowed(prior_migrations(&STATE_MIGRATOR)),
        ..Migrator::DEFAULT
    };
    before.run(&mut connection).await?;
    exercise_upgrade::<sqlx::Sqlite, _>(
        &mut connection,
        SQLITE_UPGRADE,
        async |connection: &mut sqlx::SqliteConnection| {
            // Exercise the real startup upgrade from the previous schema version.
            STATE_MIGRATOR.run(connection).await?;
            Ok(())
        },
    )
    .await?;
    connection.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; uses a transaction-local schema and rolls it back"]
async fn postgres_turn_provider_upgrade_preserves_history_and_guards() -> anyhow::Result<()> {
    // Never open the production runtime/migrator: isolate all DDL and fixtures.
    let url = std::env::var("TEST_DATABASE_URL")?;
    let mut connection = sqlx::PgConnection::connect(&url).await?;
    let mut tx = connection.begin().await?;
    let schema = format!("turn_provider_test_{}", uuid::Uuid::new_v4().simple());
    // The identifier contains only a fixed prefix and locally generated hex.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE SCHEMA {schema}; SET LOCAL search_path = {schema};"
    )))
    .execute(&mut *tx)
    .await?;
    for step in prior_migrations(&POSTGRES_STATE_MIGRATOR) {
        let sql: &'static str = step.sql.as_str();
        sqlx::raw_sql(sql).execute(&mut *tx).await?;
    }
    exercise_upgrade::<sqlx::Postgres, _>(
        &mut tx,
        POSTGRES_UPGRADE,
        async |connection: &mut sqlx::PgConnection| {
            sqlx::raw_sql(POSTGRES_UPGRADE).execute(connection).await?;
            Ok(())
        },
    )
    .await?;
    tx.rollback().await?;
    connection.close().await?;
    Ok(())
}
