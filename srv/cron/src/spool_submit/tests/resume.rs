use std::sync::Arc;

use super::*;
use chaos_vfs::MountConfig;
use sqlx::Row;

use crate::provider::BackendCronStorage;
use crate::spool_submit::saga::Direction;
use crate::spool_submit::saga::STEP_INSERT_SUBMITTED;
use crate::spool_submit::saga::STEP_SUBMIT;

#[tokio::test]
async fn restored_saga_past_submit_does_not_resubmit() {
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

    store
        .insert_queued("manifest-resume", "mock", 1, "[\"item-1\"]")
        .await
        .expect("seed queued row");
    let state = SagaState {
        direction: Direction::Forward,
        step: STEP_INSERT_SUBMITTED,
        attempt: 2,
        poll: Some(poll_wiring("resumed")),
        poll_cron_id: None,
        error: None,
    };
    store
        .save_saga(
            "manifest-resume",
            Some(&serde_json::to_string(&state).expect("encode")),
            Some("batch-from-before-restart"),
        )
        .await
        .expect("seed saga");

    let outcomes = resume_pending(&registry, &store, &cron).await;
    assert_eq!(outcomes.len(), 1);
    let (manifest_id, outcome) = &outcomes[0];
    assert_eq!(manifest_id, "manifest-resume");
    let receipt = outcome.as_ref().expect("resumed ok");
    assert_eq!(receipt.batch_id, "batch-from-before-restart");
    assert!(receipt.poll_job.is_some());

    assert!(recorder.submitted.lock().expect("poison").is_empty());
    assert!(recorder.cancelled.lock().expect("poison").is_empty());

    let row = sqlx::query(
        "SELECT status, batch_id, saga_json, submitted_at FROM spool_jobs WHERE manifest_id = ?",
    )
    .bind("manifest-resume")
    .fetch_one(&pool)
    .await
    .expect("fetch row");
    assert_eq!(row.get::<String, _>("status"), "InProgress");
    assert_eq!(
        row.get::<Option<String>, _>("batch_id").as_deref(),
        Some("batch-from-before-restart")
    );
    assert_eq!(row.get::<Option<String>, _>("saga_json"), None);
    assert!(row.get::<Option<i64>, _>("submitted_at").is_some());

    let cron_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM cron_jobs WHERE kind = 'spool' AND manifest_id = ?",
    )
    .bind("manifest-resume")
    .fetch_one(&pool)
    .await
    .expect("count cron rows");
    assert_eq!(cron_rows, 1);
}

#[tokio::test]
async fn restored_saga_without_batch_id_compensates_instead_of_resubmitting() {
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

    store
        .insert_queued("manifest-lost", "mock", 1, "[\"item-1\"]")
        .await
        .expect("seed queued row");
    let state = SagaState {
        direction: Direction::Forward,
        step: STEP_SUBMIT,
        attempt: 1,
        poll: None,
        poll_cron_id: None,
        error: None,
    };
    store
        .save_saga(
            "manifest-lost",
            Some(&serde_json::to_string(&state).expect("encode")),
            None,
        )
        .await
        .expect("seed saga");

    let outcomes = resume_pending(&registry, &store, &cron).await;
    let (_, outcome) = &outcomes[0];
    let err = outcome
        .as_ref()
        .expect_err("cannot replay without payloads");
    assert!(err.contains("interrupted"), "err={err}");
    assert!(recorder.submitted.lock().expect("poison").is_empty());
    assert!(recorder.cancelled.lock().expect("poison").is_empty());

    let row =
        sqlx::query("SELECT status, batch_id, saga_json FROM spool_jobs WHERE manifest_id = ?")
            .bind("manifest-lost")
            .fetch_one(&pool)
            .await
            .expect("fetch row");
    assert_eq!(row.get::<String, _>("status"), "Failed");
    assert_eq!(row.get::<Option<String>, _>("batch_id"), None);
    assert_eq!(row.get::<Option<String>, _>("saga_json"), None);
}
