use super::*;
use chaos_abi::SpoolBackend;
use chaos_abi::SpoolError;
use chaos_abi::SpoolItem;
use chaos_abi::SpoolPhase;
use chaos_abi::SpoolRegistry;
use chaos_abi::SpoolStatusReport;
use chaos_vfs::MountConfig;
use sqlx::Row;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;

/// Install the shared registry once per process. Subsequent test runs
/// tolerate the second install being a no-op (OnceLock semantics).
fn install_shared_registry_with_mock() {
    let mut registry = SpoolRegistry::new();
    registry.register(Arc::new(MockBackend::new()));
    // Ignore the result — tests running in the same process share the
    // single global. First install wins; follow-ups see the same mock.
    let _ = chaos_abi::set_shared_spool_registry(Arc::new(registry));
}

struct MockBackend {
    submitted: Mutex<Vec<Vec<String>>>,
}
impl MockBackend {
    fn new() -> Self {
        Self {
            submitted: Mutex::new(Vec::new()),
        }
    }
}
impl SpoolBackend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }
    fn submit(
        &self,
        items: Vec<(String, TurnRequest)>,
    ) -> Pin<Box<dyn Future<Output = Result<String, SpoolError>> + Send + '_>> {
        let ids: Vec<String> = items.into_iter().map(|(id, _)| id).collect();
        self.submitted.lock().expect("poison").push(ids);
        Box::pin(async { Ok("mock-batch-99".into()) })
    }
    fn poll(
        &self,
        _: &str,
    ) -> Pin<Box<dyn Future<Output = Result<SpoolStatusReport, SpoolError>> + Send + '_>> {
        Box::pin(async {
            Ok(SpoolStatusReport {
                phase: SpoolPhase::InProgress,
                raw_provider_status: "mock".into(),
            })
        })
    }
    fn fetch_results(
        &self,
        _: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SpoolItem>, SpoolError>> + Send + '_>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn cancel(&self, _: &str) -> Pin<Box<dyn Future<Output = Result<(), SpoolError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn spool_submit_tool_batches_and_wires_poll_cron_row() {
    install_shared_registry_with_mock();

    let tmp = tempfile::tempdir().expect("tmp");
    let provider = ChaosVfs::from_config(MountConfig::sqlite_home(tmp.path()))
        .await
        .expect("provider");
    let pool = provider.sqlite_pool().expect("sqlite pool");

    let params = SpoolSubmitParams {
        manifest_id: "manifest-tool-1".into(),
        backend: "mock".into(),
        poll_schedule: crate::schedule::Schedule::Interval { seconds: 300 },
        name: Some("nightly-review".into()),
        items: vec![
            SpoolSubmitItem {
                custom_id: "a".into(),
                model: "mock-model".into(),
                instructions: "be terse".into(),
                user_message: "hi".into(),
            },
            SpoolSubmitItem {
                custom_id: "b".into(),
                model: "mock-model".into(),
                instructions: "be terse".into(),
                user_message: "there".into(),
            },
        ],
    };
    let owner = OwnerContext {
        project_path: Some("/tmp/project".into()),
        session_id: Some("session-1".into()),
        ..Default::default()
    };

    let summary = execute_structured_on(&provider, &params, &owner)
        .await
        .expect("tool ok")
        .to_string();
    assert!(summary.contains("mock-batch-99"), "summary={summary}");
    assert!(summary.contains("manifest-tool-1"), "summary={summary}");

    // Spool row landed in InProgress with both custom ids in payload_json.
    let spool_row = sqlx::query(
        "SELECT backend, batch_id, status, request_count, payload_json \
         FROM spool_jobs WHERE manifest_id = ?",
    )
    .bind("manifest-tool-1")
    .fetch_one(&pool)
    .await
    .expect("fetch spool row");
    let status: String = spool_row.get("status");
    let request_count: i64 = spool_row.get("request_count");
    let payload_json: String = spool_row.get("payload_json");
    assert_eq!(status, "InProgress");
    assert_eq!(request_count, 2);
    assert!(payload_json.contains("\"a\"") && payload_json.contains("\"b\""));

    // Cron row created with kind=spool and the matching manifest_id.
    let cron_row = sqlx::query(
        "SELECT name, schedule, kind, manifest_id, enabled, project_path \
         FROM cron_jobs WHERE manifest_id = ?",
    )
    .bind("manifest-tool-1")
    .fetch_one(&pool)
    .await
    .expect("fetch cron row");
    let name: String = cron_row.get("name");
    let schedule: String = cron_row.get("schedule");
    let kind: String = cron_row.get("kind");
    let enabled: i64 = cron_row.get("enabled");
    let project_path: String = cron_row.get("project_path");
    assert_eq!(name, "nightly-review");
    assert_eq!(
        crate::schedule::Schedule::parse(&schedule).expect("valid schedule"),
        crate::schedule::Schedule::Interval { seconds: 300 }
    );
    assert_eq!(kind, "spool");
    assert_eq!(enabled, 1);
    assert_eq!(project_path, "/tmp/project");
}

#[tokio::test]
async fn spool_submit_tool_rejects_unknown_backend() {
    install_shared_registry_with_mock();

    let tmp = tempfile::tempdir().expect("tmp");
    let provider = ChaosVfs::from_config(MountConfig::sqlite_home(tmp.path()))
        .await
        .expect("provider");
    let pool = provider.sqlite_pool().expect("sqlite pool");

    let params = SpoolSubmitParams {
        manifest_id: "m2".into(),
        backend: "nonexistent".into(),
        poll_schedule: crate::schedule::Schedule::Interval { seconds: 300 },
        name: None,
        items: vec![SpoolSubmitItem {
            custom_id: "a".into(),
            model: "m".into(),
            instructions: String::new(),
            user_message: "x".into(),
        }],
    };
    let owner = OwnerContext {
        project_path: Some("/tmp".into()),
        session_id: Some("s".into()),
        ..Default::default()
    };

    let err = execute_structured_on(&provider, &params, &owner)
        .await
        .expect_err("should fail");
    assert!(err.contains("nonexistent"), "err={err}");

    let row = sqlx::query(
        "SELECT backend, batch_id, status, request_count, payload_json, error, submitted_at, completed_at \
         FROM spool_jobs WHERE manifest_id = ?",
    )
    .bind("m2")
    .fetch_one(&pool)
    .await
    .expect("fetch failed spool row");
    let backend_name: String = row.get("backend");
    let batch_id: Option<String> = row.get("batch_id");
    let status: String = row.get("status");
    let request_count: i64 = row.get("request_count");
    let payload_json: String = row.get("payload_json");
    let error: Option<String> = row.get("error");
    let submitted_at: Option<i64> = row.get("submitted_at");
    let completed_at: Option<i64> = row.get("completed_at");
    assert_eq!(backend_name, "nonexistent");
    assert!(batch_id.is_none());
    assert_eq!(status, "Failed");
    assert_eq!(request_count, 1);
    assert!(payload_json.contains("\"a\""));
    assert!(error.as_deref().unwrap_or_default().contains("nonexistent"));
    assert!(submitted_at.is_none());
    assert!(completed_at.is_some());
}

#[tokio::test]
async fn spool_submit_tool_replaces_old_poll_cron_rows_for_same_manifest() {
    install_shared_registry_with_mock();

    let tmp = tempfile::tempdir().expect("tmp");
    let provider = ChaosVfs::from_config(MountConfig::sqlite_home(tmp.path()))
        .await
        .expect("provider");
    let pool = provider.sqlite_pool().expect("sqlite pool");
    let owner = OwnerContext {
        project_path: Some("/tmp/project".into()),
        session_id: Some("session-1".into()),
        ..Default::default()
    };

    let first = SpoolSubmitParams {
        manifest_id: "manifest-replace".into(),
        backend: "mock".into(),
        poll_schedule: crate::schedule::Schedule::Interval { seconds: 300 },
        name: Some("first".into()),
        items: vec![SpoolSubmitItem {
            custom_id: "a".into(),
            model: "mock-model".into(),
            instructions: String::new(),
            user_message: "hi".into(),
        }],
    };
    execute_structured_on(&provider, &first, &owner)
        .await
        .expect("first submit");

    let second = SpoolSubmitParams {
        manifest_id: "manifest-replace".into(),
        backend: "mock".into(),
        poll_schedule: crate::schedule::Schedule::Interval { seconds: 900 },
        name: Some("second".into()),
        items: vec![SpoolSubmitItem {
            custom_id: "b".into(),
            model: "mock-model".into(),
            instructions: String::new(),
            user_message: "there".into(),
        }],
    };
    let summary = execute_structured_on(&provider, &second, &owner)
        .await
        .expect("second submit");
    assert_eq!(summary["replaced_poll_rows"], 1);

    let row = sqlx::query(
        "SELECT COUNT(*) AS count FROM cron_jobs WHERE kind = 'spool' AND manifest_id = ?",
    )
    .bind("manifest-replace")
    .fetch_one(&pool)
    .await
    .expect("count cron rows");
    let count: i64 = row.get("count");
    assert_eq!(count, 1);

    let cron_row = sqlx::query(
        "SELECT name, schedule FROM cron_jobs WHERE kind = 'spool' AND manifest_id = ?",
    )
    .bind("manifest-replace")
    .fetch_one(&pool)
    .await
    .expect("fetch cron row");
    let name: String = cron_row.get("name");
    let schedule: String = cron_row.get("schedule");
    assert_eq!(name, "second");
    assert_eq!(
        crate::schedule::Schedule::parse(&schedule).expect("valid schedule"),
        crate::schedule::Schedule::Interval { seconds: 900 }
    );
}
