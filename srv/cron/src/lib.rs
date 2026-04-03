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
pub(crate) use provider::CronStorage;
pub(crate) use provider::SqliteCronStorage;
pub use schedule::Schedule;
pub use scheduler::Scheduler;
pub use scheduler::shell_executor;
pub use scheduler::spawn_global as spawn_scheduler;
pub use store::CronStore;
pub use tools::create::OwnerContext;

use chaos_traits::catalog::CatalogRegistration;
use chaos_traits::catalog::tool_infos_to_catalog_tools;
use mcp_host::prelude::*;

inventory::submit! {
    CatalogRegistration {
        name: "cron",
        tools: || tool_infos_to_catalog_tools(tool_infos()),
        resources: || vec![],
        resource_templates: || vec![],
        prompts: || vec![],
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
