//! MCP resource: chaos://crons — exposes cron jobs as a readable resource.

use sqlx::SqlitePool;

use crate::store::CronStore;

/// List all cron jobs as a JSON string. Returns `"[]"` when the pool is unavailable.
pub async fn list_crons(pool: Option<&SqlitePool>) -> Result<String, String> {
    let Some(pool) = pool else {
        return Ok("[]".to_string());
    };

    let store = CronStore::new(pool.to_owned());
    let jobs = store
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
