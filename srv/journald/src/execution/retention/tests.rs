use super::*;

// Run the same failure/ownership protocol against actual backend transactions.
macro_rules! retention_scenario {
    ($pool:ident, $execute:ident, $export_fault:literal, $export_clear:literal, $seal_fault:literal, $seal_clear:literal) => {{
        let session_id = Uuid::new_v4();
        let id = session_id.to_string();
        let scope = format!("archive-{session_id}");
        let process = Uuid::new_v4().to_string();
        let turn = Uuid::new_v4().to_string();
        let event = Uuid::new_v4().to_string();
        let invocation = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO execution_scopes (scope,policy_digest) VALUES ($1,'policy')")
            .bind(&scope).execute(&$pool).await.unwrap();
        sqlx::query("INSERT INTO execution_process_bindings (process_id,scope,policy_digest) VALUES ($1,$2,'policy')")
            .bind(&process).bind(&scope).execute(&$pool).await.unwrap();
        sqlx::query(
            "INSERT INTO execution_sessions (session_id,scope,process_id,state,owner_id,lease_token,expires_at_ms)
             VALUES ($1,$2,$3,'idle','writer',$4,9999999999999)"
        ).bind(&id).bind(&scope).bind(&process).bind(Uuid::new_v4().to_string()).execute(&$pool).await.unwrap();
        let export = ExecutionRetentionRequest::Export { scope: scope.clone(), session_id };
        assert!(matches!($execute(&$pool, &export).await, Err(JournalError::ExecutionConflict(_))));
        sqlx::query("UPDATE execution_sessions SET state = 'closed' WHERE session_id = $1")
            .bind(&id).execute(&$pool).await.unwrap();
        sqlx::query(
            "INSERT INTO execution_turns (session_id,turn_id,fingerprint,state,outcome_json,accounting_enabled)
             VALUES ($1,$2,'immutable-fingerprint','completed','{\"result\":\"retained\"}',1)"
        ).bind(&id).bind(&turn).execute(&$pool).await.unwrap();
        sqlx::query(
            "INSERT INTO execution_outbox (event_id,event_key,scope,session_id,turn_id,kind,created_at_ms,body_json,next_attempt_at_ms)
             VALUES ($1,$1,$2,$3,$4,'turn.terminal',1,'{\"signed\":\"exact bytes\"}',1)"
        ).bind(&event).bind(&scope).bind(&id).bind(&turn).execute(&$pool).await.unwrap();
        assert!(matches!($execute(&$pool, &export).await, Err(JournalError::ExecutionConflict(_))));
        sqlx::query("UPDATE execution_outbox SET delivered_at_ms = 42 WHERE event_id = $1")
            .bind(&event).execute(&$pool).await.unwrap();
        sqlx::query(
            "INSERT INTO execution_invocations (invocation_id,session_id,turn_id,request_json,started_at_ms,state)
             VALUES ($1,$2,$3,'{\"provider\":\"test\"}',1,'started')"
        ).bind(&invocation).bind(&id).bind(&turn).execute(&$pool).await.unwrap();
        assert!(matches!($execute(&$pool, &export).await, Err(JournalError::ExecutionConflict(_))));
        sqlx::query("UPDATE execution_invocations SET state = 'unknown' WHERE invocation_id = $1")
            .bind(&invocation).execute(&$pool).await.unwrap();
        assert!(matches!($execute(&$pool, &export).await, Err(JournalError::ExecutionConflict(_))));
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM execution_archives WHERE session_id = $1")
            .bind(&id).fetch_one(&$pool).await.unwrap();
        assert_eq!(count, 0, "rejected exports must leave no acknowledgement target");
        sqlx::query(
            "UPDATE execution_invocations SET state = 'completed', receipt_json = '{\"usage\":null}', finished_at_ms = 2 WHERE invocation_id = $1"
        ).bind(&invocation).execute(&$pool).await.unwrap();
        let child = Uuid::new_v4().to_string();
        let source_turn = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO execution_process_bindings (process_id,scope,policy_digest) VALUES ($1,$2,'policy')")
            .bind(&child).bind(&scope).execute(&$pool).await.unwrap();
        sqlx::query(
            "INSERT INTO execution_descendants (process_id,session_id,root_turn_id,parent_process_id,registered_at_ms)
             VALUES ($1,$2,$3,$4,2)"
        ).bind(&child).bind(&id).bind(&turn).bind(&process).execute(&$pool).await.unwrap();
        sqlx::query("UPDATE execution_invocations SET source_turn_id = $1 WHERE invocation_id = $2")
            .bind(&source_turn).bind(&invocation).execute(&$pool).await.unwrap();
        sqlx::raw_sql($export_fault).execute(&$pool).await.unwrap();
        assert!($execute(&$pool, &export).await.is_err());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM execution_archives WHERE session_id = $1")
            .bind(&id).fetch_one(&$pool).await.unwrap();
        assert_eq!(count, 0, "failed export transaction cannot publish an acknowledgement target");
        sqlx::raw_sql($export_clear).execute(&$pool).await.unwrap();
        let (first, concurrent) = tokio::join!($execute(&$pool, &export), $execute(&$pool, &export));
        let first = first.unwrap();
        assert_eq!(first, concurrent.unwrap(), "concurrent export must publish one immutable snapshot");
        let manifest = first.manifest.as_ref().unwrap();
        assert_eq!(manifest.sha256, Sha256::digest(manifest.json.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>());
        let contents: Value = serde_json::from_str(&manifest.json).unwrap();
        assert_eq!(contents["turns"][0]["fingerprint"], "immutable-fingerprint");
        assert_eq!(contents["turns"][0]["accounting_coverage"], "direct_logical_sampling_and_compaction");
        assert_eq!(contents["events"][0]["body_json"], "{\"signed\":\"exact bytes\"}");
        assert_eq!(contents["invocations"][0]["receipt_json"], "{\"usage\":null}");
        assert_eq!(contents["invocations"][0]["source_turn_id"], source_turn);
        assert_eq!(contents["descendants"][0]["process_id"], child);
        assert_eq!(contents["descendants"][0]["parent_process_id"], process);
        assert_eq!(contents["descendants"][0]["scope"], scope);
        assert_eq!(contents["process_binding"]["scope"], scope);
        assert_eq!(contents["billing_complete"], false);
        assert_eq!(contents["payloads_compacted"], false);
        let wrong_scope = ExecutionRetentionRequest::Read { scope: "another-tenant".into(), session_id };
        assert!(matches!($execute(&$pool, &wrong_scope).await, Err(JournalError::ExecutionNotFound)));
        let seal = ExecutionRetentionRequest::Seal {
            scope: scope.clone(), session_id, manifest_digest: manifest.sha256.clone(),
            archive_reference: "offline-backup:operator-copy-1".into(), actor: "operator".into(),
        };
        let mut wrong_digest = seal.clone();
        if let ExecutionRetentionRequest::Seal { manifest_digest, .. } = &mut wrong_digest {
            *manifest_digest = "0".repeat(64);
        }
        assert!(matches!($execute(&$pool, &wrong_digest).await, Err(JournalError::ExecutionConflict(_))));
        let read = ExecutionRetentionRequest::Read { scope: scope.clone(), session_id };
        assert_eq!($execute(&$pool, &read).await.unwrap(), first, "failed seal must be atomic");
        // Late financial evidence invalidates the first unsealed snapshot,
        // rather than overwriting its bytes or silently claiming finality.
        sqlx::query(
            "INSERT INTO execution_price_schedules (scope,version,schedule_json,created_at_ms)
             VALUES ($1,'test-v1','{\"prices\":\"immutable\"}',2)"
        ).bind(&scope).execute(&$pool).await.unwrap();
        let reconciliation = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO execution_reconciliations (reconciliation_id,scope,session_id,turn_id,invocation_id,receipt_key,record_json,created_at_ms)
             VALUES ($1,$2,$3,$4,$5,$1,'{\"request\":{\"price_version\":\"test-v1\"},\"estimated\":null}',3)"
        ).bind(&reconciliation).bind(&scope).bind(&id).bind(&turn).bind(&invocation).execute(&$pool).await.unwrap();
        sqlx::query("INSERT INTO execution_replay (session_id,sequence) VALUES ($1,9)")
            .bind(&id).execute(&$pool).await.unwrap();
        sqlx::query(
            "INSERT INTO execution_replay_events (session_id,sequence,event_json,event_bytes)
             VALUES ($1,9,'{\"retained\":\"last observation\"}',31)"
        ).bind(&id).execute(&$pool).await.unwrap();
        assert!(matches!($execute(&$pool, &seal).await, Err(JournalError::ExecutionConflict(_))));
        // The session-only estimate fits, but the separately loaded price
        // schedule would overrun its remaining escaping/materialization budget.
        let near_limit = format!("\"{}\"", "x".repeat(MAX_EXECUTION_ARCHIVE_BYTES / 6 - 16 * 1024));
        let price_json = format!("{{\"prices\":\"{}\"}}", "x".repeat(16 * 1024 - 64));
        sqlx::query("UPDATE execution_turns SET outcome_json = $1 WHERE session_id = $2 AND turn_id = $3")
            .bind(near_limit).bind(&id).bind(&turn).execute(&$pool).await.unwrap();
        sqlx::query("UPDATE execution_price_schedules SET schedule_json = $1 WHERE scope = $2 AND version = 'test-v1'")
            .bind(price_json).bind(&scope).execute(&$pool).await.unwrap();
        assert!(matches!($execute(&$pool, &export).await, Err(JournalError::ExecutionConflict(_))),
            "referenced prices must share the pre-materialization manifest budget");
        assert_eq!($execute(&$pool, &read).await.unwrap(), first,
            "over-budget price evidence must not publish a partial export");
        sqlx::query("UPDATE execution_turns SET outcome_json = '{\"result\":\"retained\"}' WHERE session_id = $1 AND turn_id = $2")
            .bind(&id).bind(&turn).execute(&$pool).await.unwrap();
        sqlx::query("UPDATE execution_price_schedules SET schedule_json = '{\"prices\":\"immutable\"}' WHERE scope = $1 AND version = 'test-v1'")
            .bind(&scope).execute(&$pool).await.unwrap();
        let earlier = first;
        let first = $execute(&$pool, &export).await.unwrap();
        let manifest = first.manifest.as_ref().unwrap();
        assert_ne!(manifest.sha256, earlier.manifest.as_ref().unwrap().sha256);
        let contents: Value = serde_json::from_str(&manifest.json).unwrap();
        assert_eq!(contents["reconciliations"][0]["reconciliation_id"], reconciliation);
        assert_eq!(contents["price_schedules"]["test-v1"]["schedule_json"], "{\"prices\":\"immutable\"}");
        assert_eq!(contents["replay_cursor"], 9);
        assert_eq!(contents["replay_gap"], true, "bounded observations must never claim complete event history");
        assert_eq!(contents["replay_events"][0]["sequence"], 9);
        let latest_digest = manifest.sha256.clone();
        let earlier_json: String = sqlx::query_scalar("SELECT manifest_json FROM execution_archives WHERE session_id = $1 AND manifest_digest = $2")
            .bind(&id).bind(&earlier.manifest.as_ref().unwrap().sha256).fetch_one(&$pool).await.unwrap();
        assert_eq!(earlier_json, earlier.manifest.as_ref().unwrap().json);
        // Force identical timestamps: monotonic revisions, not digest ordering
        // or wall time, determine the latest exported evidence.
        sqlx::query("UPDATE execution_archives SET exported_at_ms = 7 WHERE session_id = $1")
            .bind(&id).execute(&$pool).await.unwrap();
        let mut first = first;
        first.archive.as_mut().unwrap().exported_at_ms = 7;
        assert_eq!($execute(&$pool, &read).await.unwrap(), first);
        let seal = ExecutionRetentionRequest::Seal {
            scope: scope.clone(), session_id, manifest_digest: latest_digest,
            archive_reference: "offline-backup:operator-copy-2".into(), actor: "operator".into(),
        };
        sqlx::raw_sql($seal_fault).execute(&$pool).await.unwrap();
        assert!($execute(&$pool, &seal).await.is_err());
        assert_eq!($execute(&$pool, &read).await.unwrap(), first, "failed seal commit must preserve the unsealed export");
        sqlx::raw_sql($seal_clear).execute(&$pool).await.unwrap();
        let (sealed, concurrent) = tokio::join!($execute(&$pool, &seal), $execute(&$pool, &seal));
        let sealed = sealed.unwrap();
        assert_eq!(sealed, concurrent.unwrap(), "concurrent identical seals are read-only retries");
        assert!(sealed.archive.as_ref().unwrap().sealed_at_ms.is_some());
        assert!(!sealed.archive.as_ref().unwrap().payloads_compacted);
        let mut changed_seal = seal.clone();
        if let ExecutionRetentionRequest::Seal { actor, .. } = &mut changed_seal {
            *actor = "different-operator".into();
        }
        assert!(matches!($execute(&$pool, &changed_seal).await, Err(JournalError::ExecutionConflict(_))));
        assert_eq!($execute(&$pool, &export).await.unwrap(), sealed);
        let retained: (String, Option<String>) = sqlx::query_as(
            "SELECT fingerprint,outcome_json FROM execution_turns WHERE session_id = $1 AND turn_id = $2"
        ).bind(&id).bind(&turn).fetch_one(&$pool).await.unwrap();
        assert_eq!(retained, ("immutable-fingerprint".into(), Some("{\"result\":\"retained\"}".into())));
        let binding: Option<String> = sqlx::query_scalar("SELECT scope FROM execution_process_bindings WHERE process_id = $1")
            .bind(&process).fetch_one(&$pool).await.unwrap();
        assert_eq!(binding.as_deref(), Some(scope.as_str()), "archive never releases permanent ownership");
        let oversized = format!("\"{}\"", "x".repeat(MAX_EXECUTION_ARCHIVE_BYTES / 6 + 1));
        sqlx::query("UPDATE execution_turns SET outcome_json = $1 WHERE session_id = $2 AND turn_id = $3")
            .bind(oversized).bind(&id).bind(&turn).execute(&$pool).await.unwrap();
        assert!(matches!($execute(&$pool, &export).await, Err(JournalError::ExecutionConflict(_))));
        assert_eq!($execute(&$pool, &read).await.unwrap(), sealed, "oversized evidence cannot replace or truncate retained exports");
        sqlx::query("UPDATE execution_turns SET outcome_json = '{\"result\":\"retained\"}' WHERE session_id = $1 AND turn_id = $2")
            .bind(&id).bind(&turn).execute(&$pool).await.unwrap();
        // Simulate a buggy writer changing already-exported evidence. Even if
        // storage is externally corrupted, a sealed snapshot remains immutable;
        // read-only retries cannot attest a different/newly exported payload.
        sqlx::query("UPDATE execution_invocations SET receipt_json = '{\"changed\":true}' WHERE invocation_id = $1")
            .bind(&invocation).execute(&$pool).await.unwrap();
        assert_eq!($execute(&$pool, &seal).await.unwrap(), sealed);
        assert!(matches!($execute(&$pool, &export).await, Err(JournalError::ExecutionConflict(_))));
        assert_eq!($execute(&$pool, &read).await.unwrap(), sealed, "retained export is immutable after drift");
    }};
}

#[tokio::test]
async fn sqlite_archive_is_explicit_atomic_scoped_and_non_destructive() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::SqliteJournalStore::open(&dir.path().join("retention.sqlite"))
        .await
        .unwrap();
    let pool = store.pool().clone();
    retention_scenario!(
        pool,
        execute_retention_sqlite,
        "CREATE TRIGGER retention_test_fail_export BEFORE INSERT ON execution_archives
         BEGIN SELECT RAISE(FAIL, 'injected export failure'); END",
        "DROP TRIGGER retention_test_fail_export",
        "CREATE TRIGGER retention_test_fail_seal BEFORE UPDATE ON execution_archives
         BEGIN SELECT RAISE(FAIL, 'injected seal failure'); END",
        "DROP TRIGGER retention_test_fail_seal"
    );
}

#[tokio::test]
async fn postgres_archive_is_explicit_atomic_scoped_and_non_destructive() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        return;
    };
    let pool = chaos_proc::open_runtime_db_postgres_url(&url)
        .await
        .unwrap();
    retention_scenario!(
        pool,
        execute_retention_postgres,
        "CREATE FUNCTION retention_test_fail_export() RETURNS trigger LANGUAGE plpgsql AS
         $$ BEGIN RAISE EXCEPTION 'injected export failure'; END $$;
         CREATE TRIGGER retention_test_fail_export BEFORE INSERT ON execution_archives
         FOR EACH ROW EXECUTE FUNCTION retention_test_fail_export()",
        "DROP TRIGGER retention_test_fail_export ON execution_archives; DROP FUNCTION retention_test_fail_export()",
        "CREATE FUNCTION retention_test_fail_seal() RETURNS trigger LANGUAGE plpgsql AS
         $$ BEGIN RAISE EXCEPTION 'injected seal failure'; END $$;
         CREATE TRIGGER retention_test_fail_seal BEFORE UPDATE ON execution_archives
         FOR EACH ROW EXECUTE FUNCTION retention_test_fail_seal()",
        "DROP TRIGGER retention_test_fail_seal ON execution_archives; DROP FUNCTION retention_test_fail_seal()"
    );
}
