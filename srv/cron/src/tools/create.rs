//! MCP tool: cron_create — lets the LLM schedule a cron job.

use mcp_host::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::BackendCronStorage;
use crate::CronCtx;
use crate::CronServer;
use crate::CronStorage;
use crate::job::CreateJobParams;
use chaos_storage::ChaosStorageProvider;

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
    #[mcp_tool(
        name = "cron_create",
        description = "Schedule a recurring cron job with a cron expression or interval shorthand.",
        destructive = false,
        open_world = false
    )]
    async fn cron_create(
        &self,
        ctx: CronCtx<'_>,
        params: Parameters<CronCreateParams>,
    ) -> ToolResult {
        let owner = OwnerContext {
            project_path: ctx
                .environment
                .map(|environment| environment.cwd().to_string_lossy().to_string()),
            session_id: Some(ctx.session.id.clone()),
        };
        match execute(&params.0, None, &owner).await {
            Ok(text) => Ok(ToolOutput::text(text)),
            Err(msg) => Err(ToolError::Execution(msg)),
        }
    }
}

/// Standalone execution — callable from both MCP and kernel adapter.
///
/// When `provider` is `Some`, the job is persisted to that configured
/// shared runtime backend. When `None`, standalone mode resolves storage
/// from environment.
/// Missing DB access is treated as an execution error rather than a
/// validation-only success, because cron jobs are always expected to persist.
pub async fn execute(
    params: &CronCreateParams,
    provider: Option<&ChaosStorageProvider>,
    owner: &OwnerContext,
) -> Result<String, String> {
    let provider = match provider {
        Some(provider) => provider.clone(),
        None => ChaosStorageProvider::from_env(None).await?,
    };
    let storage = BackendCronStorage::from_provider(&provider)?;
    execute_with_storage(params, &storage, owner).await
}

async fn execute_with_storage<S: CronStorage>(
    params: &CronCreateParams,
    storage: &S,
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
        crate::CronScope::Project => Some(owner.project_path.clone().ok_or_else(|| {
            "current context is missing a project path for project-scoped cron jobs".to_string()
        })?),
        _ => None,
    };
    let session_id = match scope {
        crate::CronScope::Session | crate::CronScope::Agent => {
            Some(owner.session_id.clone().ok_or_else(|| {
                "current context is missing a session id for session/agent-scoped cron jobs"
                    .to_string()
            })?)
        }
        _ => None,
    };

    let create_params = CreateJobParams::shell(
        params.name.clone(),
        params.schedule.clone(),
        params.command.clone(),
        scope,
        project_path,
        session_id,
    );

    let job = storage
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
