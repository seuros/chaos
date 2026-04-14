//! MCP tool: cron_toggle — enable, disable, or delete a cron job by ID.

use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::BackendCronStorage;
use crate::CronCtx;
use crate::CronJob;
use crate::CronScope;
use crate::CronServer;
use crate::CronStorage;
use crate::OwnerContext;
use chaos_storage::ChaosStorageProvider;

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
    #[mcp_tool(
        name = "cron_toggle",
        description = "Enable, disable, or delete an existing cron job by ID.",
        destructive = true,
        open_world = false
    )]
    async fn cron_toggle(
        &self,
        ctx: CronCtx<'_>,
        params: Parameters<CronToggleParams>,
    ) -> ToolResult {
        let owner = OwnerContext {
            project_path: ctx
                .environment
                .map(|environment| environment.cwd().to_string_lossy().to_string()),
            session_id: Some(ctx.session.id.clone()),
        };
        match execute(&params.0, None, Some(&owner)).await {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }
}

/// Standalone execution — callable from both MCP and kernel adapter.
pub async fn execute(
    params: &CronToggleParams,
    provider: Option<&ChaosStorageProvider>,
    owner: Option<&OwnerContext>,
) -> Result<String, String> {
    let provider = match provider {
        Some(provider) => provider.clone(),
        None => ChaosStorageProvider::from_env(None).await?,
    };
    let storage = BackendCronStorage::from_provider(&provider)?;
    execute_with_storage(params, &storage, owner).await
}

async fn execute_with_storage<S: CronStorage>(
    params: &CronToggleParams,
    store: &S,
    owner: Option<&OwnerContext>,
) -> Result<String, String> {
    // Verify the job exists first.
    let job = store
        .get(&params.id)
        .await
        .map_err(|e| format!("failed to look up job: {e}"))?
        .ok_or_else(|| format!("no cron job found with id: {}", params.id))?;
    enforce_owner_access(&job, owner)?;

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

fn enforce_owner_access(job: &CronJob, owner: Option<&OwnerContext>) -> Result<(), String> {
    let Some(owner) = owner else {
        return Ok(());
    };

    match job.scope {
        CronScope::Project => {
            let job_project_path = job.project_path.as_deref().ok_or_else(|| {
                format!(
                    "cron job '{}' (id: {}) is missing project ownership metadata",
                    job.name, job.id
                )
            })?;
            let owner_project_path = owner
                .project_path
                .as_deref()
                .ok_or_else(|| "current context is missing a project path".to_string())?;
            if owner_project_path != job_project_path {
                return Err(format!(
                    "cron job '{}' (id: {}) belongs to a different project",
                    job.name, job.id
                ));
            }
        }
        CronScope::Session | CronScope::Agent => {
            let job_session_id = job.session_id.as_deref().ok_or_else(|| {
                format!(
                    "cron job '{}' (id: {}) is missing session ownership metadata",
                    job.name, job.id
                )
            })?;
            let owner_session_id = owner
                .session_id
                .as_deref()
                .ok_or_else(|| "current context is missing a session id".to_string())?;
            if owner_session_id != job_session_id {
                return Err(format!(
                    "cron job '{}' (id: {}) belongs to a different session",
                    job.name, job.id
                ));
            }
        }
    }

    Ok(())
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
