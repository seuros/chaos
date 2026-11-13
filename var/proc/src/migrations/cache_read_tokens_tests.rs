use std::borrow::Cow;

use chaos_ipc::protocol::EventMsg;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::migrate::{Migration, Migrator};
use sqlx::types::Json;
use sqlx::{Connection, Row};

use super::{POSTGRES_STATE_MIGRATOR, STATE_MIGRATOR};

const PROCESS_ID: &str = "01a07d43-796f-7881-a045-fdfe8dbdb79b";
const SQLITE_VERSION: i64 = 40;
const POSTGRES_VERSION: i64 = 39;

fn usage(cached: i64) -> Value {
    json!({
        "input_tokens": 1000,
        "cached_input_tokens": cached,
        "output_tokens": 50,
        "reasoning_output_tokens": 5,
        "total_tokens": 1050
    })
}

fn fixtures() -> Vec<Value> {
    vec![
        json!({"type": "event_msg", "payload": {
            "type": "token_count",
            "info": {
                "total_token_usage": usage(700),
                "last_token_usage": usage(300),
                "model_context_window": 200000
            },
            "rate_limits": null
        }}),
        json!({"type": "event_msg", "payload": {
            "type": "token_count", "info": null, "rate_limits": null
        }}),
        json!({"type": "response_item", "payload": {
            "type": "function_call_output",
            "call_id": "c1",
            "output": "grep found cached_input_tokens"
        }}),
    ]
}

fn prior_migrations(migrator: &'static Migrator, version: i64) -> &'static [Migration] {
    let count = migrator
        .migrations
        .partition_point(|step| step.version < version);
    &migrator.migrations[..count]
}

fn check_rows(rows: Vec<Value>) {
    let mut expected = fixtures();
    for side in ["total_token_usage", "last_token_usage"] {
        let usage = &mut expected[0]["payload"]["info"][side];
        let cached = usage
            .as_object_mut()
            .and_then(|usage| usage.remove("cached_input_tokens"))
            .expect("fixture usage");
        usage["cache_read_input_tokens"] = cached;
    }
    assert_eq!(rows, expected);
    let EventMsg::TokenCount(event) =
        EventMsg::deserialize(&rows[0]["payload"]).expect("upgraded token count must load")
    else {
        panic!("not a token count");
    };
    let info = event.info.expect("info");
    assert_eq!(info.total_token_usage.cache_read_input_tokens, 700);
    assert_eq!(info.last_token_usage.cache_read_input_tokens, 300);
}

const INSERT: &str =
    "INSERT INTO journal_entries (process_id, seq, recorded_at, item_type, payload_json)
     VALUES ($1, $2, $3, $4, $5)";
const SEED_PROCESS: &str =
    "INSERT INTO processes (id, source, source_json, model_provider, cwd, created_at, updated_at)
     VALUES ($1, 'cli', '\"cli\"', 'provider', '/tmp', 10, 20)";
const SELECT: &str = "SELECT payload_json FROM journal_entries ORDER BY seq";
const GUARD: &str = "UPDATE journal_entries SET recorded_at = 999";

#[tokio::test]
async fn sqlite_journal_token_counts_move_to_cache_read_input_tokens() -> anyhow::Result<()> {
    let mut connection = sqlx::SqliteConnection::connect("sqlite::memory:").await?;
    Migrator {
        migrations: Cow::Borrowed(prior_migrations(&STATE_MIGRATOR, SQLITE_VERSION)),
        ..Migrator::DEFAULT
    }
    .run(&mut connection)
    .await?;
    sqlx::query(SEED_PROCESS)
        .bind(PROCESS_ID)
        .execute(&mut connection)
        .await?;
    for (seq, item) in fixtures().iter().enumerate() {
        sqlx::query(INSERT)
            .bind(PROCESS_ID)
            .bind(seq as i64)
            .bind(100 + seq as i64)
            .bind(item["type"].as_str())
            .bind(Json(item))
            .execute(&mut connection)
            .await?;
    }
    STATE_MIGRATOR.run(&mut connection).await?;
    let rows = sqlx::query(SELECT).fetch_all(&mut connection).await?;
    check_rows(
        rows.iter()
            .map(|row| row.get::<Json<Value>, _>(0).0)
            .collect(),
    );
    let error = sqlx::query(GUARD).execute(&mut connection).await.err();
    assert!(error.is_some_and(|error| error.to_string().contains("append-only")));
    connection.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; uses a transaction-local schema and rolls it back"]
async fn postgres_journal_token_counts_move_to_cache_read_input_tokens() -> anyhow::Result<()> {
    let url = std::env::var("TEST_DATABASE_URL")?;
    let mut connection = sqlx::PgConnection::connect(&url).await?;
    let mut tx = connection.begin().await?;
    let schema = format!("cache_read_test_{}", uuid::Uuid::new_v4().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE SCHEMA {schema}; SET LOCAL search_path = {schema};"
    )))
    .execute(&mut *tx)
    .await?;
    for step in prior_migrations(&POSTGRES_STATE_MIGRATOR, POSTGRES_VERSION) {
        sqlx::raw_sql(step.sql.as_str()).execute(&mut *tx).await?;
    }
    sqlx::query(SEED_PROCESS)
        .bind(PROCESS_ID)
        .execute(&mut *tx)
        .await?;
    for (seq, item) in fixtures().iter().enumerate() {
        sqlx::query(INSERT)
            .bind(PROCESS_ID)
            .bind(seq as i64)
            .bind(100 + seq as i64)
            .bind(item["type"].as_str())
            .bind(Json(item))
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
    check_rows(
        rows.iter()
            .map(|row| row.get::<Json<Value>, _>(0).0)
            .collect(),
    );
    sqlx::raw_sql("SAVEPOINT guard").execute(&mut *tx).await?;
    let error = sqlx::query(GUARD).execute(&mut *tx).await.err();
    assert!(error.is_some_and(|error| error.to_string().contains("append-only")));
    tx.rollback().await?;
    connection.close().await?;
    Ok(())
}
