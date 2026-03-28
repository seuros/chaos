//! MCP tool: cron_create — lets the LLM schedule a cron job.

use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::CronCtx;
use crate::CronServer;
use crate::job::CreateJobParams;
use crate::store::CronStore;

/// Parameters for the cron_create tool.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct CronCreateParams {
    /// Human-readable name for the job (e.g., "check CI status").
    pub name: String,

    /// Cron expression ("*/5 * * * *") or interval shorthand ("5m", "2h", "1d").
    pub schedule: String,

    /// The command or prompt to execute on each tick.
    pub command: String,

    /// Scope: "project" (persists across sessions), "session" (dies with session),
    /// or "agent" (self-scheduled, ephemeral).
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "project".to_string()
}

/// Owner context injected by the kernel handler — not exposed in the MCP schema.
#[derive(Debug, Default)]
pub struct OwnerContext {
    /// Current working directory for project-scoped jobs.
    pub project_path: Option<String>,
    /// Session ID for session/agent-scoped jobs.
    pub session_id: Option<String>,
}

impl CronServer {
    #[mcp_tool(name = "cron_create")]
    async fn cron_create(
        &self,
        _ctx: CronCtx<'_>,
        params: Parameters<CronCreateParams>,
    ) -> ToolResult {
        match execute(&params.0, None, &OwnerContext::default()).await {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }
}

/// Standalone execution — callable from both MCP and kernel adapter.
///
/// When `pool` is `Some`, the job is persisted to chaos.sqlite.
/// When `None`, validation-only mode (returns what would be created).
pub async fn execute(
    params: &CronCreateParams,
    pool: Option<&SqlitePool>,
    owner: &OwnerContext,
) -> Result<String, String> {
    // Validate the schedule parses
    crate::Schedule::parse(&params.schedule).map_err(|e| format!("invalid schedule: {e}"))?;

    // Validate scope
    let scope: crate::CronScope = params
        .scope
        .parse()
        .map_err(|e| format!("invalid scope: {e}"))?;

    // Derive owner metadata based on scope.
    let project_path = match scope {
        crate::CronScope::Project => owner.project_path.clone(),
        _ => None,
    };
    let session_id = match scope {
        crate::CronScope::Session | crate::CronScope::Agent => owner.session_id.clone(),
        _ => None,
    };

    let create_params = CreateJobParams {
        name: params.name.clone(),
        schedule: params.schedule.clone(),
        command: params.command.clone(),
        scope,
        project_path,
        session_id,
    };

    if let Some(pool) = pool {
        let store = CronStore::new(pool.clone());
        let job = store
            .create(&create_params)
            .await
            .map_err(|e| format!("failed to persist cron job: {e}"))?;

        Ok(format!(
            "Cron job created (id: {}):\n  name: {}\n  schedule: {}\n  command: {}\n  scope: {}\n  next_run_at: {}",
            job.id,
            job.name,
            job.schedule,
            job.command,
            job.scope,
            job.next_run_at
                .map_or("none".to_string(), |t| t.to_string()),
        ))
    } else {
        Ok(format!(
            "Cron job validated (chaos DB unavailable, not persisted):\n  name: {}\n  schedule: {}\n  command: {}\n  scope: {}",
            create_params.name, create_params.schedule, create_params.command, scope,
        ))
    }
}

/// Returns the auto-generated `ToolInfo` for schema extraction by core.
pub fn tool_info() -> ToolInfo {
    CronServer::cron_create_tool_info()
}

pub fn mount(
    router: mcp_host::registry::router::McpToolRouter<CronServer>,
) -> mcp_host::registry::router::McpToolRouter<CronServer> {
    router.with_tool(
        CronServer::cron_create_tool_info(),
        CronServer::cron_create_handler,
        None,
    )
}
