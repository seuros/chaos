mcp_host::auto_tools!(crate::CronServer, "src/tools");

use crate::BackendCronStorage;
use crate::CronCtx;
use chaos_vfs::ChaosVfs;

pub(crate) fn owner_context_from_cron_ctx(ctx: CronCtx<'_>) -> create::OwnerContext {
    create::OwnerContext {
        project_path: ctx
            .environment
            .map(|environment| environment.cwd().to_string_lossy().to_string()),
        session_id: Some(ctx.session.id.clone()),
    }
}

pub(crate) fn cron_storage() -> Result<BackendCronStorage, String> {
    Ok(BackendCronStorage::from_provider(cron_vfs()?))
}

pub(crate) fn cron_vfs() -> Result<&'static ChaosVfs, String> {
    chaos_vfs::root().map_err(|err| err.to_string())
}
