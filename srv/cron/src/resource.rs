//! MCP resource: chaos://crons — exposes cron jobs as a readable resource.

use crate::CronStorage;
use crate::SqliteCronStorage;
use chaos_storage::ChaosStorageProvider;

/// List all cron jobs as a JSON string.
pub async fn list_crons(provider: Option<&ChaosStorageProvider>) -> Result<String, String> {
    let provider = match provider {
        Some(provider) => provider.clone(),
        None => ChaosStorageProvider::from_env(None).await?,
    };
    let storage = SqliteCronStorage::from_provider(&provider)?;
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
