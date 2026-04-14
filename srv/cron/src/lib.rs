//! Chaos cron — scheduled task execution for project, session, and agent scopes.

mod job;
mod provider;
pub mod resource;
mod schedule;
mod scheduler;
mod store;
pub mod tools;

pub use job::CreateJobParams;
pub use job::CronJob;
pub use job::CronScope;
pub(crate) use provider::BackendCronStorage;
pub(crate) use provider::CronStorage;
pub use schedule::Schedule;
pub use scheduler::Scheduler;
pub use scheduler::shell_executor;
pub use scheduler::spawn_global as spawn_scheduler;
pub use store::CronStore;
pub use tools::create::OwnerContext;

use chaos_traits::catalog::CatalogRegistration;
use chaos_traits::catalog::CatalogToolDriver;
use chaos_traits::catalog::CatalogToolDriverFuture;
use chaos_traits::catalog::CatalogToolRequest;
use chaos_traits::catalog::CatalogToolResult;
use chaos_traits::catalog::tool_infos_to_catalog_tools_with_parallel;
use mcp_host::prelude::*;
use std::sync::Arc;

struct CronToolDriver;

impl CatalogToolDriver for CronToolDriver {
    fn call_tool(&self, request: CatalogToolRequest) -> CatalogToolDriverFuture<'_> {
        Box::pin(async move {
            let provider = match chaos_storage::ChaosStorageProvider::from_env(None).await {
                Ok(provider) => provider,
                Err(_) => {
                    chaos_storage::ChaosStorageProvider::from_optional_sqlite(
                        None,
                        Some(request.sqlite_home.as_path()),
                    )
                    .await?
                }
            };
            let owner = OwnerContext {
                project_path: Some(request.cwd.to_string_lossy().to_string()),
                session_id: Some(request.session_id),
            };
            let result = match request.tool_name.as_str() {
                "cron_create" => {
                    let params: tools::create::CronCreateParams =
                        serde_json::from_value(request.arguments)
                            .map_err(|e| format!("invalid arguments: {e}"))?;
                    tools::create::execute(&params, Some(&provider), &owner).await
                }
                "cron_toggle" => {
                    let params: tools::toggle::CronToggleParams =
                        serde_json::from_value(request.arguments)
                            .map_err(|e| format!("invalid arguments: {e}"))?;
                    tools::toggle::execute(&params, Some(&provider), Some(&owner)).await
                }
                other => Err(format!("unknown cron tool: {other}")),
            }?;
            Ok(CatalogToolResult {
                output: result,
                success: Some(true),
                effects: Vec::new(),
            })
        })
    }
}

fn cron_tool_driver() -> Arc<dyn CatalogToolDriver> {
    Arc::new(CronToolDriver)
}

inventory::submit! {
    CatalogRegistration {
        name: "cron",
        tools: || tool_infos_to_catalog_tools_with_parallel(tool_infos(), false),
        resources: || vec![],
        resource_templates: || vec![],
        prompts: || vec![],
        tool_driver: Some(cron_tool_driver),
    }
}

/// MCP server state for cron tools.
pub struct CronServer;

/// Shared server execution context for cron tools.
pub type CronCtx<'a> = Ctx<'a>;

/// Returns tool metadata for all cron tools.
pub fn tool_infos() -> Vec<ToolInfo> {
    vec![tools::create::tool_info(), tools::toggle::tool_info()]
}
