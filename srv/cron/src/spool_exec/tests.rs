use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use super::*;
use crate::CronScope;
use crate::job::CronJob;
use crate::job::JobKind;
use crate::provider::BackendCronStorage;
use chaos_abi::SpoolBackend;
use chaos_abi::SpoolError;
use chaos_abi::SpoolItem;
use chaos_abi::SpoolStatusReport;
use chaos_abi::TurnRequest;
use chaos_vfs::ChaosVfs;
use chaos_vfs::MountConfig;

/// Scripted backend: each poll pops the next phase from the queue.
struct MockBackend {
    phases: Mutex<Vec<SpoolPhase>>,
    results: Vec<SpoolItem>,
}

impl MockBackend {
    fn new(phases: Vec<SpoolPhase>, results: Vec<SpoolItem>) -> Self {
        Self {
            phases: Mutex::new(phases),
            results,
        }
    }
}

impl SpoolBackend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn submit(
        &self,
        _items: Vec<(String, TurnRequest)>,
    ) -> Pin<Box<dyn Future<Output = Result<String, SpoolError>> + Send + '_>> {
        Box::pin(async { Ok("mock-batch".into()) })
    }

    fn poll(
        &self,
        _batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<SpoolStatusReport, SpoolError>> + Send + '_>> {
        let next = self
            .phases
            .lock()
            .expect("poison")
            .first()
            .copied()
            .unwrap_or(SpoolPhase::Completed);
        if !self.phases.lock().expect("poison").is_empty() {
            self.phases.lock().expect("poison").remove(0);
        }
        Box::pin(async move {
            Ok(SpoolStatusReport {
                phase: next,
                raw_provider_status: format!("{next:?}"),
            })
        })
    }

    fn fetch_results(
        &self,
        _batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SpoolItem>, SpoolError>> + Send + '_>> {
        let results = self.results.clone();
        Box::pin(async move { Ok(results) })
    }

    fn cancel(
        &self,
        _batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SpoolError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

fn fake_cron_job(manifest_id: &str) -> CronJob {
    CronJob {
        id: "job-1".into(),
        name: "spool-poll".into(),
        schedule: crate::schedule::Schedule::Interval { seconds: 60 }.to_json(),
        command: String::new(),
        scope: CronScope::Project,
        project_path: None,
        session_id: None,
        enabled: true,
        last_run_at: None,
        next_run_at: None,
        created_at: 0,
        updated_at: 0,
        kind: JobKind::SPOOL_TAG.into(),
        manifest_id: Some(manifest_id.into()),
        execution_policy: None,
    }
}

async fn seed_row(pool: &sqlx::SqlitePool, manifest_id: &str) {
    sqlx::query(
        "INSERT INTO spool_jobs \
         (manifest_id, backend, batch_id, status, request_count, payload_json, created_at, updated_at) \
         VALUES (?, 'mock', 'mock-batch', 'InProgress', 1, '[]', 0, 0)",
    )
    .bind(manifest_id)
    .execute(pool)
    .await
    .expect("seed spool row");
}

async fn fetch_row(
    pool: &sqlx::SqlitePool,
    manifest_id: &str,
) -> (String, Option<String>, Option<String>) {
    let row =
        sqlx::query("SELECT status, result_json, error FROM spool_jobs WHERE manifest_id = ?")
            .bind(manifest_id)
            .fetch_one(pool)
            .await
            .expect("fetch");
    (
        sqlx::Row::get(&row, "status"),
        sqlx::Row::get(&row, "result_json"),
        sqlx::Row::get(&row, "error"),
    )
}

async fn seed_cron_row(pool: &sqlx::SqlitePool, job_id: &str, manifest_id: &str) {
    let schedule = crate::schedule::Schedule::Interval { seconds: 60 }.to_json();
    sqlx::query(
        "INSERT INTO cron_jobs \
         (id, name, schedule, command, scope, project_path, session_id, enabled, last_run_at, next_run_at, created_at, updated_at, kind, manifest_id) \
         VALUES (?, 'spool-poll', ?, '', 'project', NULL, NULL, 1, NULL, 0, 0, 0, 'spool', ?)",
    )
    .bind(job_id)
    .bind(schedule)
    .bind(manifest_id)
    .execute(pool)
    .await
    .expect("seed cron row");
}

async fn cron_enabled(pool: &sqlx::SqlitePool, job_id: &str) -> bool {
    let row = sqlx::query("SELECT enabled FROM cron_jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("fetch cron row");
    sqlx::Row::get::<i64, _>(&row, "enabled") != 0
}

#[tokio::test]
async fn spool_executor_drives_in_progress_then_completion() {
    use chaos_abi::ContentItem;
    use chaos_abi::TurnOutput;
    use chaos_abi::TurnResult;

    let tmp = tempfile::tempdir().expect("tmp");
    let provider = ChaosVfs::from_config(MountConfig::sqlite_home(tmp.path()))
        .await
        .expect("provider");
    let pool = provider.sqlite_pool().expect("sqlite pool");
    seed_row(&pool, "manifest-A").await;
    seed_cron_row(&pool, "job-1", "manifest-A").await;

    let backend = Arc::new(MockBackend::new(
        vec![SpoolPhase::InProgress, SpoolPhase::Completed],
        vec![(
            "custom-1".into(),
            TurnResult::Success(TurnOutput {
                content: vec![ContentItem::OutputText { text: "hi".into() }],
                finish_reason: Some("end_turn".into()),
                usage: None,
                server_model: Some("mock".into()),
            }),
        )],
    ));
    let mut registry = SpoolRegistry::new();
    registry.register(backend);
    let registry = Arc::new(registry);

    let executor = spool_executor_from_provider(registry, &provider);
    let job = fake_cron_job("manifest-A");

    // Tick 1: InProgress → row unchanged.
    let msg = executor(&job).await.expect("tick ok");
    assert!(msg.contains("in_progress"), "msg={msg}");
    let (status, result_json, error) = fetch_row(&pool, "manifest-A").await;
    assert_eq!(status, "InProgress");
    assert!(result_json.is_none());
    assert!(error.is_none());

    // Tick 2: Completed → row updated with results.
    let msg = executor(&job).await.expect("tick ok");
    assert!(msg.contains("completed"), "msg={msg}");
    let (status, result_json, error) = fetch_row(&pool, "manifest-A").await;
    assert_eq!(status, "Completed");
    let payload = result_json.expect("results persisted");
    assert!(payload.contains("custom-1"), "payload={payload}");
    assert!(error.is_none());
    assert!(!cron_enabled(&pool, "job-1").await);

    // Tick 3: already terminal → no-op.
    let msg = executor(&job).await.expect("tick ok");
    assert!(msg.contains("already Completed"), "msg={msg}");
}

#[tokio::test]
async fn spool_executor_persists_failure_and_error_message() {
    let tmp = tempfile::tempdir().expect("tmp");
    let provider = ChaosVfs::from_config(MountConfig::sqlite_home(tmp.path()))
        .await
        .expect("provider");
    let pool = provider.sqlite_pool().expect("sqlite pool");
    let store = BackendSpoolStore::from_provider(&provider);
    let cron_store = BackendCronStorage::from_provider(&provider);
    seed_row(&pool, "manifest-B").await;
    seed_cron_row(&pool, "job-1", "manifest-B").await;

    let backend = Arc::new(MockBackend::new(vec![SpoolPhase::Failed], vec![]));
    let mut registry = SpoolRegistry::new();
    registry.register(backend);
    let registry = Arc::new(registry);

    let executor = spool_executor(registry, store, cron_store);
    let job = fake_cron_job("manifest-B");

    executor(&job).await.expect("tick ok");
    let (status, result_json, error) = fetch_row(&pool, "manifest-B").await;
    assert_eq!(status, "Failed");
    assert!(result_json.is_none());
    assert!(error.is_some());
    assert!(!cron_enabled(&pool, "job-1").await);
}
