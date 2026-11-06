use std::sync::Arc;

use super::*;
use chaos_vfs::MountConfig;
use sqlx::Row;

use crate::provider::BackendCronStorage;

#[tokio::test]
async fn submit_manifest_persists_in_progress_row_with_batch_id() {
    let tmp = tempfile::tempdir().expect("tmp");
    let provider = ChaosVfs::from_config(MountConfig::sqlite_home(tmp.path()))
        .await
        .expect("provider");
    let pool = provider.sqlite_pool().expect("sqlite pool");
    let store = BackendSpoolStore::from_provider(&provider);
    let cron = BackendCronStorage::from_provider(&provider);

    let backend = Arc::new(RecordingBackend::new());
    let recorder = backend.clone();
    let mut registry = SpoolRegistry::new();
    registry.register(backend);

    let items = vec![
        ("item-1".to_string(), dummy_turn_request("mock-1")),
        ("item-2".to_string(), dummy_turn_request("mock-2")),
    ];
    let receipt = submit_manifest(&registry, &store, &cron, "manifest-Z", "mock", items, None)
        .await
        .expect("submit ok");
    assert_eq!(receipt.batch_id, "mock-batch-42");
    assert!(receipt.poll_job.is_none());

    // Backend saw both items, in order.
    let submitted = recorder.submitted.lock().expect("poison").clone();
    assert_eq!(submitted, vec![vec!["item-1".to_string(), "item-2".into()]]);

    // Row landed in InProgress with the custom ids captured in payload_json.
    let row = sqlx::query(
        "SELECT backend, batch_id, status, request_count, payload_json, submitted_at \
         FROM spool_jobs WHERE manifest_id = ?",
    )
    .bind("manifest-Z")
    .fetch_one(&pool)
    .await
    .expect("fetch row");
    let status: String = row.get("status");
    let backend_name: String = row.get("backend");
    let batch_id_col: String = row.get("batch_id");
    let request_count: i64 = row.get("request_count");
    let payload_json: String = row.get("payload_json");
    let submitted_at: Option<i64> = row.get("submitted_at");
    assert_eq!(status, "InProgress");
    assert_eq!(backend_name, "mock");
    assert_eq!(batch_id_col, "mock-batch-42");
    assert_eq!(request_count, 2);
    assert!(payload_json.contains("item-1") && payload_json.contains("item-2"));
    assert!(submitted_at.is_some());
}

#[tokio::test]
async fn submit_manifest_persists_failures_for_missing_and_failing_backends() {
    let tmp = tempfile::tempdir().expect("tmp");
    let provider = ChaosVfs::from_config(MountConfig::sqlite_home(tmp.path()))
        .await
        .expect("provider");
    let pool = provider.sqlite_pool().expect("sqlite pool");
    let store = BackendSpoolStore::from_provider(&provider);
    let cron = BackendCronStorage::from_provider(&provider);
    let mut registry = SpoolRegistry::new();
    registry.register(Arc::new(FailingBackend));

    for (manifest, backend, message) in [
        ("manifest-missing", "ghost", "ghost"),
        ("manifest-submit-fails", "failing", "submit exploded"),
    ] {
        let err = submit_manifest(
            &registry,
            &store,
            &cron,
            manifest,
            backend,
            vec![("x".into(), dummy_turn_request("m"))],
            None,
        )
        .await
        .expect_err(backend);
        assert!(err.contains(message), "{backend}: {err}");

        let row = sqlx::query(
            "SELECT backend, batch_id, status, request_count, payload_json, error, \
                    submitted_at, completed_at \
             FROM spool_jobs WHERE manifest_id = ?",
        )
        .bind(manifest)
        .fetch_one(&pool)
        .await
        .expect("fetch failed row");
        assert_eq!(row.get::<String, _>("backend"), backend);
        assert_eq!(row.get::<Option<String>, _>("batch_id"), None);
        assert_eq!(row.get::<String, _>("status"), "Failed");
        assert_eq!(row.get::<i64, _>("request_count"), 1);
        assert!(row.get::<String, _>("payload_json").contains("x"));
        assert!(
            row.get::<Option<String>, _>("error")
                .as_deref()
                .unwrap_or_default()
                .contains(message),
            "{backend}: persisted error must retain the failure reason"
        );
        assert_eq!(row.get::<Option<i64>, _>("submitted_at"), None);
        assert!(row.get::<Option<i64>, _>("completed_at").is_some());
    }
}

#[tokio::test]
async fn submit_manifest_replaces_terminal_payload_with_fresh_in_progress_state() {
    let tmp = tempfile::tempdir().expect("tmp");
    let provider = ChaosVfs::from_config(MountConfig::sqlite_home(tmp.path()))
        .await
        .expect("provider");
    let pool = provider.sqlite_pool().expect("sqlite pool");
    let store = BackendSpoolStore::from_provider(&provider);
    let cron = BackendCronStorage::from_provider(&provider);

    sqlx::query(
        "INSERT INTO spool_jobs \
         (manifest_id, backend, batch_id, status, request_count, payload_json, \
          result_json, raw_result, error, submitted_at, completed_at, created_at, updated_at) \
         VALUES (?, 'old-backend', 'old-batch', 'Completed', 1, '[\"old-id\"]', \
                 '[\"old-result\"]', 'old-raw', 'old-error', 10, 20, 111, 222)",
    )
    .bind("manifest-reuse")
    .execute(&pool)
    .await
    .expect("seed old row");

    let backend = Arc::new(RecordingBackend::new());
    let mut registry = SpoolRegistry::new();
    registry.register(backend);

    submit_manifest(
        &registry,
        &store,
        &cron,
        "manifest-reuse",
        "mock",
        vec![("item-1".into(), dummy_turn_request("mock-1"))],
        None,
    )
    .await
    .expect("submit ok");

    let row = sqlx::query(
        "SELECT backend, batch_id, status, request_count, payload_json, result_json, \
                raw_result, error, submitted_at, completed_at, created_at \
         FROM spool_jobs WHERE manifest_id = ?",
    )
    .bind("manifest-reuse")
    .fetch_one(&pool)
    .await
    .expect("fetch row");

    assert_eq!(row.get::<String, _>("backend"), "mock");
    assert_eq!(row.get::<String, _>("batch_id"), "mock-batch-42");
    assert_eq!(row.get::<String, _>("status"), "InProgress");
    assert_eq!(row.get::<i64, _>("request_count"), 1);
    assert_eq!(row.get::<String, _>("payload_json"), "[\"item-1\"]");
    assert_eq!(row.get::<Option<String>, _>("result_json"), None);
    assert_eq!(row.get::<Option<String>, _>("raw_result"), None);
    assert_eq!(row.get::<Option<String>, _>("error"), None);
    assert!(row.get::<Option<i64>, _>("submitted_at").is_some());
    assert_eq!(row.get::<Option<i64>, _>("completed_at"), None);
    assert_eq!(row.get::<i64, _>("created_at"), 111);
}

#[tokio::test]
async fn submit_manifest_rejects_empty_batches() {
    let tmp = tempfile::tempdir().expect("tmp");
    let provider = ChaosVfs::from_config(MountConfig::sqlite_home(tmp.path()))
        .await
        .expect("provider");
    let store = BackendSpoolStore::from_provider(&provider);
    let cron = BackendCronStorage::from_provider(&provider);
    let backend = Arc::new(RecordingBackend::new());
    let mut registry = SpoolRegistry::new();
    registry.register(backend);

    let err = submit_manifest(
        &registry,
        &store,
        &cron,
        "manifest-empty",
        "mock",
        vec![],
        None,
    )
    .await
    .expect_err("empty batches should fail");
    assert!(err.contains("no items"), "err={err}");
}
