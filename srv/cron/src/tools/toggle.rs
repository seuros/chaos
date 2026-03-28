//! MCP tool: cron_toggle — enable, disable, or delete a cron job by ID.

use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::CronCtx;
use crate::CronServer;
use crate::store::CronStore;

/// Parameters for the cron_toggle tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct CronToggleParams {
    /// The cron job ID (short hex string from cron_create).
    id: String,

    /// Action to perform: "enable", "disable", or "delete".
    action: String,
}

impl CronServer {
    #[mcp_tool(name = "cron_toggle")]
    async fn cron_toggle(
        &self,
        _ctx: CronCtx<'_>,
        params: Parameters<CronToggleParams>,
    ) -> ToolResult {
        match execute(&params.0, None).await {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }
}

/// Standalone execution — callable from both MCP and kernel adapter.
pub async fn execute(
    params: &CronToggleParams,
    pool: Option<&SqlitePool>,
) -> Result<String, String> {
    let Some(pool) = pool else {
        return Err("chaos DB unavailable — cannot toggle cron jobs".to_string());
    };

    let store = CronStore::new(pool.to_owned());

    // Verify the job exists first.
    let job = store
        .get(&params.id)
        .await
        .map_err(|e| format!("failed to look up job: {e}"))?
        .ok_or_else(|| format!("no cron job found with id: {}", params.id))?;

    match params.action.as_str() {
        "enable" => {
            store
                .set_enabled(&params.id, true)
                .await
                .map_err(|e| format!("failed to enable job: {e}"))?;
            Ok(format!("Cron job '{}' (id: {}) enabled", job.name, job.id))
        }
        "disable" => {
            store
                .set_enabled(&params.id, false)
                .await
                .map_err(|e| format!("failed to disable job: {e}"))?;
            Ok(format!("Cron job '{}' (id: {}) disabled", job.name, job.id))
        }
        "delete" => {
            store
                .delete(&params.id)
                .await
                .map_err(|e| format!("failed to delete job: {e}"))?;
            Ok(format!("Cron job '{}' (id: {}) deleted", job.name, job.id))
        }
        other => Err(format!(
            "unknown action: '{other}' — expected 'enable', 'disable', or 'delete'"
        )),
    }
}

/// Returns the auto-generated `ToolInfo` for schema extraction by core.
pub fn tool_info() -> ToolInfo {
    CronServer::cron_toggle_tool_info()
}

pub fn mount(
    router: mcp_host::registry::router::McpToolRouter<CronServer>,
) -> mcp_host::registry::router::McpToolRouter<CronServer> {
    router.with_tool(
        CronServer::cron_toggle_tool_info(),
        CronServer::cron_toggle_handler,
        None,
    )
}
