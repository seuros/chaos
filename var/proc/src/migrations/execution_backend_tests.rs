use std::borrow::Cow;

use sqlx::Connection;
use sqlx::migrate::Migrator;

use super::STATE_MIGRATOR;

async fn migrate_through(
    connection: &mut sqlx::SqliteConnection,
    version: i64,
) -> Result<(), sqlx::migrate::MigrateError> {
    let migrations: Vec<_> = STATE_MIGRATOR
        .iter()
        .filter(|step| step.version <= version)
        .cloned()
        .collect();
    Migrator {
        migrations: Cow::Owned(migrations),
        ..Migrator::DEFAULT
    }
    .run(connection)
    .await
}

#[tokio::test]
async fn sqlite_fresh_and_upgrade_keep_execution_evidence_without_financial_schema()
-> anyhow::Result<()> {
    // Version 32 already has sessions, invocation receipts and archival.
    // Also upgrade databases that already recorded the original billing step.
    for version in [0, 32, 33, 34] {
        let mut connection = sqlx::SqliteConnection::connect("sqlite::memory:").await?;
        migrate_through(&mut connection, version).await?;
        if version >= 32 {
            sqlx::raw_sql(
                "INSERT INTO execution_sessions
                 (session_id,scope,state,owner_id,lease_token,expires_at_ms)
                 VALUES ('session','local','closed','owner','lease',1);
                 INSERT INTO execution_turns
                 (session_id,turn_id,fingerprint,state,outcome_json,accounting_enabled)
                 VALUES ('session','turn','original','completed','{\"result\":\"retained\"}',1);
                 INSERT INTO execution_invocations
                 (invocation_id,session_id,turn_id,request_json,started_at_ms,state,receipt_json)
                 VALUES ('invocation','session','turn','{\"provider\":\"live\"}',1,
                         'completed','{\"usage\":{\"total_tokens\":17}}');
                 INSERT INTO execution_provider_receipts (receipt_key,invocation_id)
                 VALUES ('provider-receipt','invocation');",
            )
            .execute(&mut connection)
            .await?;
        }
        STATE_MIGRATOR.run(&mut connection).await?;
        let financial: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table'
             AND name IN ('execution_price_schedules','execution_reconciliations')",
        )
        .fetch_all(&mut connection)
        .await?;
        assert!(financial.is_empty());
        let billing_version: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 33")
                .fetch_one(&mut connection)
                .await?;
        assert_eq!(billing_version, 1);
        // Startup still validates the original checksum after table retirement.
        STATE_MIGRATOR.run(&mut connection).await?;
        if version >= 32 {
            let evidence: (String, String, String, String) = sqlx::query_as(
                "SELECT t.fingerprint,t.outcome_json,t.accounting_coverage,i.receipt_json
                 FROM execution_turns t JOIN execution_invocations i
                 ON i.session_id = t.session_id AND i.turn_id = t.turn_id
                 JOIN execution_provider_receipts r ON r.invocation_id = i.invocation_id
                 WHERE r.receipt_key = 'provider-receipt'",
            )
            .fetch_one(&mut connection)
            .await?;
            assert_eq!(
                evidence,
                (
                    "original".into(),
                    "{\"result\":\"retained\"}".into(),
                    "direct_logical_sampling_and_compaction".into(),
                    "{\"usage\":{\"total_tokens\":17}}".into(),
                )
            );
        }
        connection.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn sqlite_billing_retirement_refuses_to_destroy_existing_financial_evidence()
-> anyhow::Result<()> {
    for reconciliation in [false, true] {
        let mut connection = sqlx::SqliteConnection::connect("sqlite::memory:").await?;
        migrate_through(&mut connection, 34).await?;
        sqlx::raw_sql(
            "INSERT INTO execution_scopes (scope,policy_digest) VALUES ('local','policy');
             INSERT INTO execution_sessions
             (session_id,scope,state,owner_id,lease_token,expires_at_ms)
             VALUES ('session','local','closed','owner','lease',1);
             INSERT INTO execution_turns (session_id,turn_id,fingerprint,state)
             VALUES ('session','turn','original','completed');
             INSERT INTO execution_invocations
             (invocation_id,session_id,turn_id,request_json,started_at_ms,state)
             VALUES ('invocation','session','turn','{}',1,'completed');",
        )
        .execute(&mut connection)
        .await?;
        let (table, evidence) = if reconciliation {
            sqlx::query(
                "INSERT INTO execution_reconciliations
                 (reconciliation_id,scope,session_id,turn_id,invocation_id,
                  receipt_key,record_json,created_at_ms)
                 VALUES ('external','local','session','turn','invocation','receipt',
                         '{\"charged\":\"retained\"}',1)",
            )
            .execute(&mut connection)
            .await?;
            ("execution_reconciliations", "{\"charged\":\"retained\"}")
        } else {
            sqlx::query(
                "INSERT INTO execution_price_schedules (scope,version,schedule_json,created_at_ms)
                 VALUES ('local','v1','{\"prices\":\"retained\"}',1)",
            )
            .execute(&mut connection)
            .await?;
            ("execution_price_schedules", "{\"prices\":\"retained\"}")
        };
        // Repeated startup failures must roll back DDL and preserve exact bytes.
        for _ in 0..2 {
            let error = STATE_MIGRATOR.run(&mut connection).await.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("export_financial_evidence_before_retiring_sqlite_billing"),
                "{error}"
            );
            let saved: String = if reconciliation {
                sqlx::query_scalar("SELECT record_json FROM execution_reconciliations")
            } else {
                sqlx::query_scalar("SELECT schedule_json FROM execution_price_schedules")
            }
            .fetch_one(&mut connection)
            .await?;
            assert_eq!(saved, evidence, "{table} evidence was lost");
            let applied: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 35")
                    .fetch_one(&mut connection)
                    .await?;
            assert_eq!(applied, 0);
        }
        connection.close().await?;
    }
    Ok(())
}
