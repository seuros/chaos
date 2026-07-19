//! Chaos cron — scheduled task execution for project, session, and agent scopes.

mod job;
mod provider;
pub mod resource;
mod schedule;
mod scheduler;
mod spool_exec;
mod spool_store;
mod spool_submit;
mod store;
pub mod tools;

pub use job::CreateJobParams;
pub use job::CronJob;
pub use job::CronScope;
pub use job::JobKind;
pub(crate) use provider::BackendCronStorage;
pub(crate) use provider::CronStorage;
pub use schedule::Schedule;
pub use scheduler::JobExecutor;
pub use scheduler::Scheduler;
pub use scheduler::dispatch_executor;
pub use scheduler::shell_executor;
pub use scheduler::spawn_global as spawn_scheduler;
pub use spool_exec::spool_executor_from_provider;
pub use spool_submit::submit_manifest_from_provider;
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
                    tools::create::execute_structured(&params, Some(&provider), &owner)
                        .await
                        .map(|value| value.to_string())
                }
                "cron_toggle" => {
                    let params: tools::toggle::CronToggleParams =
                        serde_json::from_value(request.arguments)
                            .map_err(|e| format!("invalid arguments: {e}"))?;
                    tools::toggle::execute_structured(&params, Some(&provider), Some(&owner))
                        .await
                        .map(|value| value.to_string())
                }
                "spool_submit" => {
                    let params: tools::spool_submit::SpoolSubmitParams =
                        serde_json::from_value(request.arguments)
                            .map_err(|e| format!("invalid arguments: {e}"))?;
                    tools::spool_submit::execute_structured(&params, Some(&provider), &owner)
                        .await
                        .map(|value| value.to_string())
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
        tools: || {
            let spool_available = chaos_abi::shared_spool_registry().is_some_and(|r| !r.is_empty());
            tool_infos_to_catalog_tools_with_parallel(catalog_tool_infos(spool_available), false)
        },
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
    tools::router().list()
}

/// Tool metadata as exposed to the catalog. `spool_submit` only appears when
/// at least one spool backend was registered at kernel boot; without a
/// backend the tool can never succeed, so it is not offered at all.
fn catalog_tool_infos(spool_available: bool) -> Vec<ToolInfo> {
    tool_infos()
        .into_iter()
        .filter(|tool| spool_available || tool.name != "spool_submit")
        .collect()
}

#[cfg(test)]
mod catalog_visibility_tests {
    use super::catalog_tool_infos;

    #[test]
    fn spool_submit_hidden_without_backends_and_listed_with_them() {
        let names = |infos: Vec<mcp_host::prelude::ToolInfo>| {
            infos.into_iter().map(|t| t.name).collect::<Vec<_>>()
        };
        let without = names(catalog_tool_infos(false));
        assert!(!without.contains(&"spool_submit".to_string()));
        assert!(without.contains(&"cron_create".to_string()));
        let with = names(catalog_tool_infos(true));
        assert!(with.contains(&"spool_submit".to_string()));
    }
}
