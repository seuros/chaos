mcp_host::auto_tools!(crate::CronServer, "src/tools");

use crate::BackendCronStorage;
use crate::CronCtx;
use chaos_vfs::ChaosVfs;

pub(crate) fn structured_tool_result(
    result: Result<serde_json::Value, String>,
) -> mcp_host::prelude::ToolResult {
    use mcp_host::prelude::{ToolError, ToolOutput};

    match result {
        Ok(value) => ToolOutput::structured(value)
            .map_err(|e| ToolError::Execution(format!("non-object tool output: {e}"))),
        Err(msg) => Err(ToolError::Execution(msg)),
    }
}

pub(crate) fn owner_context_from_cron_ctx(ctx: CronCtx<'_>) -> create::OwnerContext {
    create::OwnerContext {
        project_path: ctx
            .environment
            .map(|environment| environment.cwd().to_string_lossy().to_string()),
        session_id: Some(ctx.session.id().to_owned()),
        execution_policy: None,
    }
}

pub(crate) fn cron_storage() -> Result<BackendCronStorage, String> {
    Ok(BackendCronStorage::from_provider(cron_vfs()?))
}

pub(crate) fn cron_vfs() -> Result<&'static ChaosVfs, String> {
    chaos_vfs::root().map_err(|err| err.to_string())
}

#[cfg(test)]
#[path = "tools/result_tests/tests.rs"]
mod tests;
