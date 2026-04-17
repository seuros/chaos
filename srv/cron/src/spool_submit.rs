//! Queued → InProgress submit path.
//!
//! Callers hand over a batch of `(custom_id, TurnRequest)` items plus the
//! backend they want to target. We dispatch to the backend, persist the
//! returned `batch_id` into `spool_jobs`, and hand the `batch_id` back so the
//! caller can wire a cron row (`kind=spool`, `manifest_id=…`) to drive the
//! subsequent poll lifecycle.

use chaos_abi::SpoolRegistry;
use chaos_abi::TurnRequest;
use chaos_storage::ChaosStorageProvider;

use crate::spool_store::BackendSpoolStore;

/// Submit a batch of turns to `backend_name` and persist a fresh
/// `spool_jobs` row in `InProgress` state.
///
/// Returns the backend-assigned `batch_id`.
pub(crate) async fn submit_manifest(
    registry: &SpoolRegistry,
    store: &BackendSpoolStore,
    manifest_id: &str,
    backend_name: &str,
    items: Vec<(String, TurnRequest)>,
) -> Result<String, String> {
    if items.is_empty() {
        return Err(format!("manifest {manifest_id} has no items to submit"));
    }

    let request_count = u32::try_from(items.len())
        .map_err(|_| format!("too many items for manifest {manifest_id}"))?;
    let custom_ids: Vec<&str> = items.iter().map(|(id, _)| id.as_str()).collect();
    let payload_json = serde_json::to_string(&custom_ids)
        .map_err(|e| format!("serialize custom ids for {manifest_id}: {e}"))?;

    store
        .insert_queued(manifest_id, backend_name, request_count, &payload_json)
        .await
        .map_err(|e| format!("persist queued submit {manifest_id}: {e}"))?;

    let backend = match registry.get(backend_name) {
        Some(backend) => backend,
        None => {
            let msg = format!("no spool backend registered for {backend_name}");
            let _ = store.mark_submit_failed(manifest_id, &msg).await;
            return Err(msg);
        }
    };

    let batch_id = match backend.submit(items).await {
        Ok(batch_id) => batch_id,
        Err(e) => {
            let msg = format!("submit {manifest_id}: {e}");
            let _ = store.mark_submit_failed(manifest_id, &msg).await;
            return Err(msg);
        }
    };

    store
        .insert_submitted(
            manifest_id,
            backend_name,
            &batch_id,
            request_count,
            &payload_json,
        )
        .await
        .map_err(|e| format!("persist submit {manifest_id}: {e}"))?;

    Ok(batch_id)
}

/// Convenience wrapper: build the store from a provider and submit.
pub async fn submit_manifest_from_provider(
    registry: &SpoolRegistry,
    provider: &ChaosStorageProvider,
    manifest_id: &str,
    backend_name: &str,
    items: Vec<(String, TurnRequest)>,
) -> Result<String, String> {
    let store = BackendSpoolStore::from_provider(provider)?;
    submit_manifest(registry, &store, manifest_id, backend_name, items).await
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::Mutex;

    use super::*;
    use chaos_abi::SpoolBackend;
    use chaos_abi::SpoolError;
    use chaos_abi::SpoolItem;
    use chaos_abi::SpoolPhase;
    use chaos_abi::SpoolStatusReport;
    use chaos_abi::TurnRequest;
    use sqlx::Row;

    struct RecordingBackend {
        submitted: Mutex<Vec<Vec<String>>>,
    }

    impl RecordingBackend {
        fn new() -> Self {
            Self {
                submitted: Mutex::new(Vec::new()),
            }
        }
    }

    struct FailingBackend;

    impl SpoolBackend for RecordingBackend {
        fn name(&self) -> &'static str {
            "mock"
        }

        fn submit(
            &self,
            items: Vec<(String, TurnRequest)>,
        ) -> Pin<Box<dyn Future<Output = Result<String, SpoolError>> + Send + '_>> {
            let ids: Vec<String> = items.into_iter().map(|(id, _)| id).collect();
            self.submitted.lock().expect("poison").push(ids);
            Box::pin(async { Ok("mock-batch-42".into()) })
        }

        fn poll(
            &self,
            _batch_id: &str,
        ) -> Pin<Box<dyn Future<Output = Result<SpoolStatusReport, SpoolError>> + Send + '_>>
        {
            Box::pin(async {
                Ok(SpoolStatusReport {
                    phase: SpoolPhase::InProgress,
                    raw_provider_status: "mock".into(),
                })
            })
        }

        fn fetch_results(
            &self,
            _batch_id: &str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<SpoolItem>, SpoolError>> + Send + '_>> {
            Box::pin(async { Ok(vec![]) })
        }

        fn cancel(
            &self,
            _batch_id: &str,
        ) -> Pin<Box<dyn Future<Output = Result<(), SpoolError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }
    }

    impl SpoolBackend for FailingBackend {
        fn name(&self) -> &'static str {
            "failing"
        }

        fn submit(
            &self,
            _items: Vec<(String, TurnRequest)>,
        ) -> Pin<Box<dyn Future<Output = Result<String, SpoolError>> + Send + '_>> {
            Box::pin(async { Err(SpoolError::Other("submit exploded".into())) })
        }

        fn poll(
            &self,
            _batch_id: &str,
        ) -> Pin<Box<dyn Future<Output = Result<SpoolStatusReport, SpoolError>> + Send + '_>>
        {
            Box::pin(async {
                Ok(SpoolStatusReport {
                    phase: SpoolPhase::InProgress,
                    raw_provider_status: "unreachable".into(),
                })
            })
        }

        fn fetch_results(
            &self,
            _batch_id: &str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<SpoolItem>, SpoolError>> + Send + '_>> {
            Box::pin(async { Ok(vec![]) })
        }

        fn cancel(
            &self,
            _batch_id: &str,
        ) -> Pin<Box<dyn Future<Output = Result<(), SpoolError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn dummy_turn_request(model: &str) -> TurnRequest {
        TurnRequest {
            model: model.into(),
            instructions: String::new(),
            input: vec![],
            tools: vec![],
            parallel_tool_calls: false,
            reasoning: None,
            output_schema: None,
            verbosity: None,
            turn_state: None,
            extensions: serde_json::Map::new(),
        }
    }

    #[tokio::test]
    async fn submit_manifest_persists_in_progress_row_with_batch_id() {
        let tmp = tempfile::tempdir().expect("tmp");
        let provider = ChaosStorageProvider::from_optional_sqlite(None, Some(tmp.path()))
            .await
            .expect("provider");
        let pool = provider.sqlite_pool_cloned().expect("sqlite pool");
        let store = BackendSpoolStore::from_provider(&provider).expect("store");

        let backend = Arc::new(RecordingBackend::new());
        let recorder = backend.clone();
        let mut registry = SpoolRegistry::new();
        registry.register(backend);

        let items = vec![
            ("item-1".to_string(), dummy_turn_request("mock-1")),
            ("item-2".to_string(), dummy_turn_request("mock-2")),
        ];
        let batch_id = submit_manifest(&registry, &store, "manifest-Z", "mock", items)
            .await
            .expect("submit ok");
        assert_eq!(batch_id, "mock-batch-42");

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
    async fn submit_manifest_errors_when_backend_not_registered() {
        let tmp = tempfile::tempdir().expect("tmp");
        let provider = ChaosStorageProvider::from_optional_sqlite(None, Some(tmp.path()))
            .await
            .expect("provider");
        let pool = provider.sqlite_pool_cloned().expect("sqlite pool");
        let store = BackendSpoolStore::from_provider(&provider).expect("store");
        let registry = SpoolRegistry::new();

        let err = submit_manifest(
            &registry,
            &store,
            "manifest-missing",
            "ghost",
            vec![("x".into(), dummy_turn_request("m"))],
        )
        .await
        .expect_err("should fail");
        assert!(err.contains("ghost"), "err={err}");

        let row = sqlx::query(
            "SELECT backend, batch_id, status, request_count, payload_json, error, submitted_at, completed_at \
             FROM spool_jobs WHERE manifest_id = ?",
        )
        .bind("manifest-missing")
        .fetch_one(&pool)
        .await
        .expect("fetch row");
        let backend_name: String = row.get("backend");
        let batch_id: Option<String> = row.get("batch_id");
        let status: String = row.get("status");
        let request_count: i64 = row.get("request_count");
        let payload_json: String = row.get("payload_json");
        let error: Option<String> = row.get("error");
        let submitted_at: Option<i64> = row.get("submitted_at");
        let completed_at: Option<i64> = row.get("completed_at");
        assert_eq!(backend_name, "ghost");
        assert!(batch_id.is_none());
        assert_eq!(status, "Failed");
        assert_eq!(request_count, 1);
        assert!(payload_json.contains("x"));
        assert!(error.as_deref().unwrap_or_default().contains("ghost"));
        assert!(submitted_at.is_none());
        assert!(completed_at.is_some());
    }

    #[tokio::test]
    async fn submit_manifest_persists_failed_row_when_backend_submit_errors() {
        let tmp = tempfile::tempdir().expect("tmp");
        let provider = ChaosStorageProvider::from_optional_sqlite(None, Some(tmp.path()))
            .await
            .expect("provider");
        let pool = provider.sqlite_pool_cloned().expect("sqlite pool");
        let store = BackendSpoolStore::from_provider(&provider).expect("store");
        let mut registry = SpoolRegistry::new();
        registry.register(Arc::new(FailingBackend));

        let err = submit_manifest(
            &registry,
            &store,
            "manifest-submit-fails",
            "failing",
            vec![("x".into(), dummy_turn_request("m"))],
        )
        .await
        .expect_err("should fail");
        assert!(err.contains("submit exploded"), "err={err}");

        let row = sqlx::query(
            "SELECT backend, batch_id, status, request_count, payload_json, error, submitted_at, completed_at \
             FROM spool_jobs WHERE manifest_id = ?",
        )
        .bind("manifest-submit-fails")
        .fetch_one(&pool)
        .await
        .expect("fetch row");
        let backend_name: String = row.get("backend");
        let batch_id: Option<String> = row.get("batch_id");
        let status: String = row.get("status");
        let request_count: i64 = row.get("request_count");
        let payload_json: String = row.get("payload_json");
        let error: Option<String> = row.get("error");
        let submitted_at: Option<i64> = row.get("submitted_at");
        let completed_at: Option<i64> = row.get("completed_at");
        assert_eq!(backend_name, "failing");
        assert!(batch_id.is_none());
        assert_eq!(status, "Failed");
        assert_eq!(request_count, 1);
        assert!(payload_json.contains("x"));
        assert!(
            error
                .as_deref()
                .unwrap_or_default()
                .contains("submit exploded")
        );
        assert!(submitted_at.is_none());
        assert!(completed_at.is_some());
    }

    #[tokio::test]
    async fn submit_manifest_replaces_terminal_payload_with_fresh_in_progress_state() {
        let tmp = tempfile::tempdir().expect("tmp");
        let provider = ChaosStorageProvider::from_optional_sqlite(None, Some(tmp.path()))
            .await
            .expect("provider");
        let pool = provider.sqlite_pool_cloned().expect("sqlite pool");
        let store = BackendSpoolStore::from_provider(&provider).expect("store");

        sqlx::query(
            "INSERT INTO spool_jobs \
             (manifest_id, backend, batch_id, status, request_count, payload_json, result_json, raw_result, error, submitted_at, completed_at, created_at, updated_at) \
             VALUES (?, 'old-backend', 'old-batch', 'Completed', 1, '[\"old-id\"]', '[\"old-result\"]', 'old-raw', 'old-error', 10, 20, 111, 222)",
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
            "manifest-reuse",
            "mock",
            vec![("item-1".into(), dummy_turn_request("mock-1"))],
        )
        .await
        .expect("submit ok");

        let row = sqlx::query(
            "SELECT backend, batch_id, status, request_count, payload_json, result_json, raw_result, error, submitted_at, completed_at, created_at \
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
        let provider = ChaosStorageProvider::from_optional_sqlite(None, Some(tmp.path()))
            .await
            .expect("provider");
        let store = BackendSpoolStore::from_provider(&provider).expect("store");
        let backend = Arc::new(RecordingBackend::new());
        let mut registry = SpoolRegistry::new();
        registry.register(backend);

        let err = submit_manifest(&registry, &store, "manifest-empty", "mock", vec![])
            .await
            .expect_err("empty batches should fail");
        assert!(err.contains("no items"), "err={err}");
    }
}
