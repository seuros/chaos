//! MCP resources: `chaos://crons` and `chaos://spool`.

use crate::BackendCronStorage;
use crate::CronStorage;
use crate::spool_store::BackendSpoolStore;
use chaos_storage::ChaosStorageProvider;

/// List all cron jobs as a JSON string.
pub async fn list_crons(provider: Option<&ChaosStorageProvider>) -> Result<String, String> {
    let provider = match provider {
        Some(provider) => provider.clone(),
        None => ChaosStorageProvider::from_env(None).await?,
    };
    let storage = BackendCronStorage::from_provider(&provider)?;
    list_crons_with_storage(&storage).await
}

async fn list_crons_with_storage<S: CronStorage>(storage: &S) -> Result<String, String> {
    let jobs = storage
        .list(None, None)
        .await
        .map_err(|e| format!("failed to list cron jobs: {e}"))?;

    let items: Vec<serde_json::Value> = jobs
        .iter()
        .map(|j| {
            serde_json::json!({
                "id": j.id,
                "name": j.name,
                "schedule": j.schedule,
                "command": j.command,
                "scope": j.scope.as_str(),
                "project_path": j.project_path,
                "session_id": j.session_id,
                "enabled": j.enabled,
                "last_run_at": j.last_run_at,
                "next_run_at": j.next_run_at,
            })
        })
        .collect();

    serde_json::to_string_pretty(&items).map_err(|e| format!("failed to serialize cron jobs: {e}"))
}

/// List all spool jobs as a JSON string.
pub async fn list_spool(provider: Option<&ChaosStorageProvider>) -> Result<String, String> {
    let provider = match provider {
        Some(provider) => provider.clone(),
        None => ChaosStorageProvider::from_env(None).await?,
    };
    let storage = BackendSpoolStore::from_provider(&provider)?;
    let rows = storage
        .list()
        .await
        .map_err(|e| format!("failed to list spool jobs: {e}"))?;

    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "manifest_id": row.manifest_id,
                "backend": row.backend,
                "batch_id": row.batch_id,
                "status": row.status,
                "request_count": row.request_count,
                "payload_json": row.payload_json,
                "result_json": row.result_json,
                "raw_result": row.raw_result,
                "error": row.error,
                "submitted_at": row.submitted_at,
                "completed_at": row.completed_at,
                "created_at": row.created_at,
                "updated_at": row.updated_at,
            })
        })
        .collect();

    serde_json::to_string_pretty(&items).map_err(|e| format!("failed to serialize spool jobs: {e}"))
}
