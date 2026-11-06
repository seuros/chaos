use std::sync::Arc;

use super::*;
use chaos_vfs::MountConfig;
use sqlx::Row;

use crate::provider::BackendCronStorage;

struct FailInsertSubmitted<'a>(StoreEffects<'a>);

impl SubmitEffects for FailInsertSubmitted<'_> {
    async fn insert_queued(&mut self) -> Result<(), String> {
        self.0.insert_queued().await
    }
    async fn submit(&mut self) -> Result<String, String> {
        self.0.submit().await
    }
    async fn insert_submitted(&mut self, _batch_id: &str) -> Result<(), String> {
        Err("disk full".into())
    }
    async fn create_poll_row(&mut self, poll: &PollWiring) -> Result<CronJob, String> {
        self.0.create_poll_row(poll).await
    }
    async fn retire_poll_rows(&mut self, keep: &str) -> Result<u64, String> {
        self.0.retire_poll_rows(keep).await
    }
    async fn cancel(&mut self, batch_id: &str) -> Result<(), String> {
        self.0.cancel(batch_id).await
    }
    async fn mark_failed(&mut self, error: &str) -> Result<(), String> {
        self.0.mark_failed(error).await
    }
    async fn delete_poll_row(&mut self, job_id: &str) -> Result<(), String> {
        self.0.delete_poll_row(job_id).await
    }
    async fn persist(
        &mut self,
        state: Option<&SagaState>,
        batch_id: Option<&str>,
    ) -> Result<(), String> {
        self.0.persist(state, batch_id).await
    }
}

#[tokio::test]
async fn failed_insert_submitted_cancels_the_provider_batch() {
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

    let effects = StoreEffects::new(
        &registry,
        &store,
        &cron,
        "manifest-comp",
        "mock",
        vec![("item-1".into(), dummy_turn_request("mock-1"))],
    )
    .expect("effects");
    let mut effects = FailInsertSubmitted(effects);
    let err = drive(
        &mut effects,
        SubmitRun::fresh("manifest-comp", Some(poll_wiring("comp"))),
    )
    .await
    .expect_err("insert_submitted failure must abort");
    assert!(err.contains("disk full"), "err={err}");

    assert_eq!(
        recorder.cancelled.lock().expect("poison").clone(),
        vec!["mock-batch-42".to_string()]
    );
    assert_eq!(recorder.submitted.lock().expect("poison").len(), 1);

    let row = sqlx::query(
        "SELECT status, batch_id, error, saga_json FROM spool_jobs WHERE manifest_id = ?",
    )
    .bind("manifest-comp")
    .fetch_one(&pool)
    .await
    .expect("fetch row");
    assert_eq!(row.get::<String, _>("status"), "Failed");
    assert_eq!(
        row.get::<Option<String>, _>("batch_id").as_deref(),
        Some("mock-batch-42")
    );
    assert!(
        row.get::<Option<String>, _>("error")
            .unwrap_or_default()
            .contains("disk full")
    );
    assert_eq!(row.get::<Option<String>, _>("saga_json"), None);

    let cron_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cron_jobs")
        .fetch_one(&pool)
        .await
        .expect("count cron rows");
    assert_eq!(cron_rows, 0);
}

#[tokio::test]
async fn completed_submit_clears_saga_and_wires_poll_row() {
    let tmp = tempfile::tempdir().expect("tmp");
    let provider = ChaosVfs::from_config(MountConfig::sqlite_home(tmp.path()))
        .await
        .expect("provider");
    let pool = provider.sqlite_pool().expect("sqlite pool");
    let store = BackendSpoolStore::from_provider(&provider);
    let cron = BackendCronStorage::from_provider(&provider);
    let mut registry = SpoolRegistry::new();
    registry.register(Arc::new(RecordingBackend::new()));

    let receipt = submit_manifest(
        &registry,
        &store,
        &cron,
        "manifest-wired",
        "mock",
        vec![("item-1".into(), dummy_turn_request("mock-1"))],
        Some(poll_wiring("wired")),
    )
    .await
    .expect("submit ok");
    let job = receipt.poll_job.expect("poll row");
    assert_eq!(job.manifest_id.as_deref(), Some("manifest-wired"));

    let saga_json: Option<String> =
        sqlx::query_scalar("SELECT saga_json FROM spool_jobs WHERE manifest_id = ?")
            .bind("manifest-wired")
            .fetch_one(&pool)
            .await
            .expect("fetch saga");
    assert_eq!(saga_json, None);
}
