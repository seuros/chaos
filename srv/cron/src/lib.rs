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

use mcp_host::prelude::*;

/// MCP server state for cron tools.
pub struct CronServer;

/// Lightweight context wrapper — mirrors the arsenal pattern.
#[derive(Clone)]
pub struct CronCtx<'a> {
    inner: Ctx<'a>,
}

impl<'a> FromExecutionContext<'a> for CronCtx<'a> {
    fn from_execution_context(ctx: &'a ExecutionContext<'a>) -> Self {
        Self {
            inner: Ctx::from_execution_context(ctx),
        }
    }
}

impl<'a> std::ops::Deref for CronCtx<'a> {
    type Target = Ctx<'a>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Returns tool metadata for all cron tools.
pub fn tool_infos() -> Vec<ToolInfo> {
    vec![tools::create::tool_info(), tools::toggle::tool_info()]
}
