mcp_host::auto_tools!(crate::CronServer, "src/tools");

use crate::BackendCronStorage;
use crate::CronCtx;
use chaos_storage::ChaosStorageProvider;

pub(crate) fn owner_context_from_cron_ctx(ctx: CronCtx<'_>) -> create::OwnerContext {
    create::OwnerContext {
        project_path: ctx
            .environment
            .map(|environment| environment.cwd().to_string_lossy().to_string()),
        session_id: Some(ctx.session.id.clone()),
    }
}

pub(crate) async fn cron_storage_from_optional_provider(
    provider: Option<&ChaosStorageProvider>,
) -> Result<(ChaosStorageProvider, BackendCronStorage), String> {
    let provider = resolve_cron_provider(provider).await?;
    let storage = BackendCronStorage::from_provider(&provider)?;
    Ok((provider, storage))
}

pub(crate) async fn resolve_cron_provider(
    provider: Option<&ChaosStorageProvider>,
) -> Result<ChaosStorageProvider, String> {
    match provider {
        Some(provider) => Ok(provider.clone()),
        None => ChaosStorageProvider::from_env(None).await,
    }
}
