use crate::config::Config;
use crate::models_manager::cache::ModelsCacheManager;
use crate::path_utils::normalize_for_path_comparison;
use crate::rollout::list::Cursor;
use crate::rollout::list::ProcessSortKey;
use crate::rollout::metadata;
use chaos_ipc::ProcessId;
use chaos_ipc::dynamic_tools::DynamicToolSpec;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionSource;
use chaos_parrot::endpoint::batches::AnthropicSpoolBackend;
use chaos_parrot::endpoint::batches::XaiSpoolBackend;
pub use chaos_proc::LogEntry;
use chaos_proc::ProcessMetadataBuilder;
pub use chaos_proc::RuntimeDbHandle;
use chaos_storage::ChaosStorageProvider;
use jiff::Timestamp;
use serde_json::Value;
use sqlx::SqlitePool;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;
use uuid::Uuid;

/// Initialize the runtime DB for thread persistence. To only be used
/// inside `core`. The initialization should not be done anywhere else.
pub(crate) async fn init(config: &Config) -> Option<RuntimeDbHandle> {
    let provider = match resolve_runtime_storage_provider(None, config.sqlite_home.as_path()).await
    {
        Ok(provider) => provider,
        Err(err) => {
            warn!(
                "failed to initialize runtime storage for {}: {err}",
                config.sqlite_home.display()
            );
            return None;
        }
    };

    let runtime = match runtime_handle_from_provider(
        &provider,
        config.sqlite_home.clone(),
        config.model_provider_id.clone(),
    )
    .await
    {
        Ok(runtime) => runtime,
        Err(err) => {
            warn!(
                "failed to initialize runtime db at {}: {err}",
                config.sqlite_home.display()
            );
            return None;
        }
    };

    if let Err(err) = chaos_cron::spawn_scheduler(
        &provider,
        scheduler_executor(&provider, config.sqlite_home.as_path()).await,
    ) {
        warn!("failed to initialize cron scheduler storage backend: {err}");
    }

    // Install the shared ration usage store so adapters built later in
    // boot can attach sniffers via `chaos_libration::registry::sniffer_for`.
    // A store already installed (repeated init, tests) is a no-op.
    match chaos_libration::store::UsageStore::from_provider(&provider) {
        Some(store) => {
            let _ = chaos_libration::registry::set_shared_store(store);
        }
        None => warn!("failed to install ration store: storage provider exposes no pool"),
    }

    Some(runtime)
}

async fn scheduler_executor(
    provider: &ChaosStorageProvider,
    sqlite_home: &Path,
) -> chaos_cron::JobExecutor {
    let shell = chaos_cron::shell_executor();
    let registry = spool_registry_from_env(sqlite_home).await;
    if registry.is_empty() {
        return shell;
    }

    let registry = Arc::new(registry);
    // Publish the same registry to process-wide callers (MCP tools, CLI
    // subcommands) so they reach the same backends kern booted with. A
    // registry already installed is a no-op — tests and repeated init tolerate
    // the second install being dropped.
    let _ = chaos_abi::set_shared_spool_registry(registry.clone());

    let spool = match chaos_cron::spool_executor_from_provider(registry, provider) {
        Ok(executor) => executor,
        Err(err) => {
            warn!("spool backends configured, but spool execution is unavailable: {err}");
            return shell;
        }
    };

    chaos_cron::dispatch_executor(shell, spool)
}

async fn spool_registry_from_env(sqlite_home: &Path) -> chaos_abi::SpoolRegistry {
    let mut registry = chaos_abi::SpoolRegistry::new();
    let cache = ModelsCacheManager::new(sqlite_home.to_path_buf(), Duration::from_secs(3600));

    if let Some(api_key) = non_empty_env("ANTHROPIC_API_KEY") {
        let model = if let Some(m) = non_empty_env("ANTHROPIC_SPOOL_MODEL") {
            Some(m)
        } else {
            cache.first_model_id("anthropic").await
        };
        match model {
            Some(m) => registry.register(Arc::new(AnthropicSpoolBackend::new(api_key, m))),
            None => warn!(
                "ANTHROPIC_API_KEY set but no spool model resolved; fetch models or set ANTHROPIC_SPOOL_MODEL"
            ),
        }
    }

    if let Some(api_key) = non_empty_env("XAI_API_KEY") {
        let model = if let Some(m) = non_empty_env("XAI_SPOOL_MODEL") {
            Some(m)
        } else {
            cache.first_model_id("xai").await
        };
        match model {
            Some(m) => registry.register(Arc::new(XaiSpoolBackend::new(api_key, m))),
            None => warn!(
                "XAI_API_KEY set but no spool model resolved; fetch models or set XAI_SPOOL_MODEL"
            ),
        }
    }

    registry
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn runtime_handle_from_provider(
    provider: &ChaosStorageProvider,
    chaos_home: PathBuf,
    default_provider: String,
) -> anyhow::Result<RuntimeDbHandle> {
    if let Some(pool) = provider.sqlite_pool_cloned() {
        return Ok(RuntimeDbHandle::from_sqlite_pool(
            chaos_home,
            default_provider,
            pool,
        ));
    }
    if let Some(pool) = provider.postgres_pool_cloned() {
        return Ok(RuntimeDbHandle::from_postgres_pool(
            chaos_home,
            default_provider,
            pool,
        ));
    }
    anyhow::bail!("unsupported runtime storage backend")
}

/// Resolve the shared runtime storage provider, preferring explicit environment
/// configuration and otherwise falling back to the configured SQLite home.
pub async fn resolve_runtime_storage_provider(
    existing_pool: Option<&SqlitePool>,
    sqlite_home: &Path,
) -> Result<ChaosStorageProvider, String> {
    match ChaosStorageProvider::from_env(existing_pool).await {
        Ok(provider) => Ok(provider),
        Err(_) => {
            ChaosStorageProvider::from_optional_sqlite(existing_pool, Some(sqlite_home)).await
        }
    }
}

/// Get the DB if the feature is enabled and the DB exists.
pub async fn get_runtime_db(config: &Config) -> Option<RuntimeDbHandle> {
    get_runtime_db_for(config.sqlite_home.as_path(), &config.model_provider_id).await
}

/// Trait-friendly variant: accepts only the fields needed to open the runtime DB.
pub async fn get_runtime_db_for(
    sqlite_home: &Path,
    model_provider_id: &str,
) -> Option<RuntimeDbHandle> {
    if let Ok(provider) = ChaosStorageProvider::from_env(None).await {
        return runtime_handle_from_provider(
            &provider,
            sqlite_home.to_path_buf(),
            model_provider_id.to_string(),
        )
        .await
        .ok();
    }

    let state_path = chaos_proc::runtime_db_path(sqlite_home);
    if !tokio::fs::try_exists(&state_path).await.unwrap_or(false) {
        return None;
    }

    let provider = ChaosStorageProvider::from_optional_sqlite(None, Some(sqlite_home))
        .await
        .ok()?;
    runtime_handle_from_provider(
        &provider,
        sqlite_home.to_path_buf(),
        model_provider_id.to_string(),
    )
    .await
    .ok()
}

/// Open the runtime DB when the backing store appears present, without feature gating.
pub async fn open_if_present(chaos_home: &Path, default_provider: &str) -> Option<RuntimeDbHandle> {
    if let Ok(provider) = ChaosStorageProvider::from_env(None).await {
        return runtime_handle_from_provider(
            &provider,
            chaos_home.to_path_buf(),
            default_provider.to_string(),
        )
        .await
        .ok();
    }

    let db_path = chaos_proc::runtime_db_path(chaos_home);
    if !tokio::fs::try_exists(&db_path).await.unwrap_or(false) {
        return None;
    }

    let provider = ChaosStorageProvider::from_optional_sqlite(None, Some(chaos_home))
        .await
        .ok()?;
    runtime_handle_from_provider(
        &provider,
        chaos_home.to_path_buf(),
        default_provider.to_string(),
    )
    .await
    .ok()
}

fn cursor_to_anchor(cursor: Option<&Cursor>) -> Option<chaos_proc::Anchor> {
    let cursor = cursor?;
    let value = serde_json::to_value(cursor).ok()?;
    let cursor_str = value.as_str()?;
    let (ts_str, id_str) = cursor_str.split_once('|')?;
    if id_str.contains('|') {
        return None;
    }
    let id = Uuid::parse_str(id_str).ok()?;
    let ts = if let Some(ts) = parse_filename_timestamp(ts_str) {
        ts
    } else if let Ok(ts) = ts_str.parse::<Timestamp>() {
        Timestamp::from_second(ts.as_second()).unwrap_or(ts)
    } else {
        return None;
    };
    Some(chaos_proc::Anchor { ts, id })
}

fn parse_filename_timestamp(ts_str: &str) -> Option<Timestamp> {
    if ts_str.len() < 19 {
        return None;
    }
    let normalized = format!(
        "{}-{}-{}T{}:{}:{}Z",
        &ts_str[0..4],
        &ts_str[5..7],
        &ts_str[8..10],
        &ts_str[11..13],
        &ts_str[14..16],
        &ts_str[17..19],
    );
    let ts: Timestamp = normalized.parse().ok()?;
    Some(Timestamp::from_second(ts.as_second()).unwrap_or(ts))
}

pub(crate) fn normalize_cwd_for_runtime_db(cwd: &Path) -> PathBuf {
    normalize_for_path_comparison(cwd).unwrap_or_else(|_| cwd.to_path_buf())
}

#[allow(clippy::too_many_arguments)]
pub async fn list_process_ids_db(
    context: Option<&RuntimeDbHandle>,
    chaos_home: &Path,
    page_size: usize,
    cursor: Option<&Cursor>,
    sort_key: ProcessSortKey,
    allowed_sources: &[SessionSource],
    model_providers: Option<&[String]>,
    archived_only: bool,
    stage: &str,
) -> Option<Vec<ProcessId>> {
    let ctx = context?;
    if ctx.chaos_home() != chaos_home {
        warn!(
            "runtime db chaos_home mismatch: expected {}, got {}",
            ctx.chaos_home().display(),
            chaos_home.display()
        );
    }

    let anchor = cursor_to_anchor(cursor);
    let allowed_sources: Vec<String> = allowed_sources
        .iter()
        .map(|value| match serde_json::to_value(value) {
            Ok(Value::String(s)) => s,
            Ok(other) => other.to_string(),
            Err(_) => String::new(),
        })
        .collect();
    let model_providers = model_providers.map(<[String]>::to_vec);
    match ctx
        .list_process_ids(
            page_size,
            anchor.as_ref(),
            match sort_key {
                ProcessSortKey::CreatedAt => chaos_proc::SortKey::CreatedAt,
                ProcessSortKey::UpdatedAt => chaos_proc::SortKey::UpdatedAt,
            },
            allowed_sources.as_slice(),
            model_providers.as_deref(),
            archived_only,
        )
        .await
    {
        Ok(ids) => Some(ids),
        Err(err) => {
            warn!("runtime db list_process_ids failed during {stage}: {err}");
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn list_processes_db(
    context: Option<&RuntimeDbHandle>,
    chaos_home: &Path,
    page_size: usize,
    cursor: Option<&Cursor>,
    sort_key: ProcessSortKey,
    allowed_sources: &[SessionSource],
    model_providers: Option<&[String]>,
    archived: bool,
    search_term: Option<&str>,
) -> Option<chaos_proc::ProcessesPage> {
    let ctx = context?;
    if ctx.chaos_home() != chaos_home {
        warn!(
            "runtime db chaos_home mismatch: expected {}, got {}",
            ctx.chaos_home().display(),
            chaos_home.display()
        );
    }

    let anchor = cursor_to_anchor(cursor);
    let allowed_sources: Vec<String> = allowed_sources
        .iter()
        .map(|value| match serde_json::to_value(value) {
            Ok(Value::String(s)) => s,
            Ok(other) => other.to_string(),
            Err(_) => String::new(),
        })
        .collect();
    let model_providers = model_providers.map(<[String]>::to_vec);
    match ctx
        .list_processes(
            page_size,
            anchor.as_ref(),
            match sort_key {
                ProcessSortKey::CreatedAt => chaos_proc::SortKey::CreatedAt,
                ProcessSortKey::UpdatedAt => chaos_proc::SortKey::UpdatedAt,
            },
            allowed_sources.as_slice(),
            model_providers.as_deref(),
            archived,
            search_term,
        )
        .await
    {
        Ok(page) => Some(page),
        Err(err) => {
            warn!("runtime db list_processes failed: {err}");
            None
        }
    }
}

pub async fn get_dynamic_tools(
    context: Option<&RuntimeDbHandle>,
    process_id: ProcessId,
    stage: &str,
) -> Option<Vec<DynamicToolSpec>> {
    let ctx = context?;
    match ctx.get_dynamic_tools(process_id).await {
        Ok(tools) => tools,
        Err(err) => {
            warn!("runtime db get_dynamic_tools failed during {stage}: {err}");
            None
        }
    }
}

pub async fn persist_dynamic_tools(
    context: Option<&RuntimeDbHandle>,
    process_id: ProcessId,
    tools: Option<&[DynamicToolSpec]>,
    stage: &str,
) {
    let Some(ctx) = context else {
        return;
    };
    if let Err(err) = ctx.persist_dynamic_tools(process_id, tools).await {
        warn!("runtime db persist_dynamic_tools failed during {stage}: {err}");
    }
}

pub async fn mark_process_memory_mode_polluted(
    context: Option<&RuntimeDbHandle>,
    process_id: ProcessId,
    stage: &str,
) {
    let Some(ctx) = context else {
        return;
    };
    if let Err(err) = ctx.mark_process_memory_mode_polluted(process_id).await {
        warn!("runtime db mark_process_memory_mode_polluted failed during {stage}: {err}");
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn apply_rollout_items(
    context: Option<&RuntimeDbHandle>,
    _default_provider: &str,
    builder: Option<&ProcessMetadataBuilder>,
    items: &[RolloutItem],
    stage: &str,
    new_process_memory_mode: Option<&str>,
    updated_at_override: Option<Timestamp>,
) {
    let Some(ctx) = context else {
        return;
    };
    let mut builder = match builder {
        Some(builder) => builder.clone(),
        None => match metadata::builder_from_items(items) {
            Some(builder) => builder,
            None => {
                warn!("runtime db apply_rollout_items missing builder during {stage}");
                warn!(
                    "runtime db discrepancy during apply_rollout_items: {stage}, missing_builder"
                );
                return;
            }
        },
    };
    builder.cwd = normalize_cwd_for_runtime_db(&builder.cwd);
    if let Err(err) = ctx
        .apply_rollout_items(
            &builder,
            items,
            new_process_memory_mode,
            updated_at_override,
        )
        .await
    {
        warn!("runtime db apply_rollout_items failed during {stage}: {err}");
    }
}

pub async fn touch_process_updated_at(
    context: Option<&RuntimeDbHandle>,
    process_id: Option<ProcessId>,
    updated_at: Timestamp,
    stage: &str,
) -> bool {
    let Some(ctx) = context else {
        return false;
    };
    let Some(process_id) = process_id else {
        return false;
    };
    ctx.touch_process_updated_at(process_id, updated_at)
        .await
        .unwrap_or_else(|err| {
            warn!(
                "runtime db touch_process_updated_at failed during {stage} for {process_id}: {err}"
            );
            false
        })
}

#[cfg(test)]
#[path = "runtime_db_tests.rs"]
mod tests;
