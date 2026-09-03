use crate::async_breaker::AsyncCircuitBreaker;
use crate::async_breaker::BreakerError;
use crate::config::Config;
use crate::path_utils::normalize_for_path_comparison;
use crate::rollout::health::RuntimeStorageBackend;
use crate::rollout::health::set_runtime_storage_backend;
use crate::rollout::list::Cursor;
use crate::rollout::list::ProcessSortKey;
use crate::rollout::metadata;
use chaos_ipc::ProcessId;
use chaos_ipc::dynamic_tools::DynamicToolSpec;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionSource;
use chaos_model_catalog::ModelsCacheManager;
use chaos_parrot::endpoint::batches::AnthropicSpoolBackend;
use chaos_parrot::endpoint::batches::XaiSpoolBackend;
pub use chaos_proc::LogEntry;
use chaos_proc::ProcessMetadataBuilder;
pub use chaos_proc::RuntimeDbHandle;
use chaos_vfs::ChaosVfs;
use chaos_vfs::Vfs;
use jiff::Timestamp;
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;
use tracing::warn;
use uuid::Uuid;

const RUNTIME_STORAGE_HALF_OPEN_TIMEOUT: Duration = Duration::from_secs(30);

static RUNTIME_STORAGE_BREAKER: LazyLock<AsyncCircuitBreaker> = LazyLock::new(|| {
    AsyncCircuitBreaker::new(
        "runtime-storage",
        1,
        Duration::from_secs(60),
        RUNTIME_STORAGE_HALF_OPEN_TIMEOUT,
        1,
    )
});

pub(crate) async fn with_runtime_storage_breaker<T, E, F, Fut>(op: F) -> Result<T, BreakerError<E>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    RUNTIME_STORAGE_BREAKER.call(op).await
}

/// Initialize the runtime DB for thread persistence. To only be used
/// inside `core`. The initialization should not be done anywhere else.
pub(crate) async fn init(config: &Config) -> anyhow::Result<Option<RuntimeDbHandle>> {
    let Some(vfs) = mount_vfs_for_startup(config).await? else {
        return Ok(None);
    };

    let runtime = runtime_handle_from_vfs(
        vfs,
        config.sqlite_home.clone(),
        config.model_provider_id.clone(),
    );

    if let Err(err) = chaos_cron::spawn_scheduler(
        vfs,
        scheduler_executor(vfs, config.sqlite_home.as_path()).await,
    ) {
        warn!("failed to initialize cron scheduler storage backend: {err}");
    }

    // Install the shared ration usage store so adapters built later in
    // boot can attach sniffers via `chaos_libration::registry::sniffer_for`.
    // A store already installed (repeated init, tests) is a no-op.
    let _ = chaos_libration::registry::set_shared_store(
        chaos_libration::store::UsageStore::from_provider(vfs),
    );

    Ok(Some(runtime))
}

async fn scheduler_executor(provider: &ChaosVfs, sqlite_home: &Path) -> chaos_cron::JobExecutor {
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

fn runtime_handle_from_vfs(
    vfs: &ChaosVfs,
    chaos_home: PathBuf,
    default_provider: String,
) -> RuntimeDbHandle {
    set_runtime_storage_backend(RuntimeStorageBackend::from(vfs.kind()));
    match vfs.pool() {
        Vfs::Sqlite(pool) => RuntimeDbHandle::from_sqlite_pool(chaos_home, default_provider, pool),
        Vfs::Postgres(pool) => {
            RuntimeDbHandle::from_postgres_pool(chaos_home, default_provider, pool)
        }
    }
}

/// Mount the storage backend named by the config, `CHAOS_STORAGE_URL`, or the
/// chaos home, in that order. Idempotent, so every entry point may call it and
/// the process still opens exactly one pool.
pub async fn mount_vfs(config: &Config) -> anyhow::Result<&'static ChaosVfs> {
    mount_root_for(config.storage_url.as_deref(), config.sqlite_home.as_path()).await
}

/// Mount runtime storage for an application entry point.
///
/// SQLite preserves the historical degraded-startup behavior. PostgreSQL is an
/// explicit operator choice, so a failed PostgreSQL mount is fatal and must
/// never fall through to the SQLite journald sidecar.
pub async fn mount_vfs_for_startup(config: &Config) -> anyhow::Result<Option<&'static ChaosVfs>> {
    let mount_config =
        chaos_vfs::resolve_mount_config(config.storage_url.as_deref(), &config.sqlite_home)?;
    match mount_vfs(config).await {
        Ok(vfs) => Ok(Some(vfs)),
        Err(err) if mount_config.kind() == chaos_vfs::VfsKind::Postgres => Err(err),
        Err(err) => {
            warn!(
                error = %err,
                "runtime storage is unavailable; continuing without database-backed services"
            );
            Ok(None)
        }
    }
}

/// Trait-friendly variant: accepts only the fields needed to mount.
pub async fn mount_root_for(
    storage_url: Option<&str>,
    sqlite_home: &Path,
) -> anyhow::Result<&'static ChaosVfs> {
    let vfs = mount_vfs_for(storage_url, sqlite_home).await?;
    chaos_vfs::set_root(vfs);
    Ok(vfs)
}

/// Open the backend without claiming the root, for callers that run before boot
/// has settled which one the process serves.
pub async fn mount_vfs_for(
    storage_url: Option<&str>,
    sqlite_home: &Path,
) -> anyhow::Result<&'static ChaosVfs> {
    let config = chaos_vfs::resolve_mount_config(storage_url, sqlite_home)?;
    if let Some(vfs) = chaos_vfs::mounted(&config) {
        return Ok(vfs);
    }

    let vfs = match with_runtime_storage_breaker(|| ChaosVfs::from_config(config.clone())).await {
        Ok(vfs) => vfs,
        Err(BreakerError::Open) => {
            anyhow::bail!("runtime storage circuit is open; database probe is backing off")
        }
        Err(BreakerError::Operation(err)) => return Err(err.into()),
    };
    Ok(chaos_vfs::mount(config, vfs))
}

/// Open the runtime DB handle, mounting the backend if boot has not yet.
pub async fn open_or_create_runtime_db(
    sqlite_home: &Path,
    default_provider: &str,
) -> anyhow::Result<RuntimeDbHandle> {
    open_or_create_runtime_db_with_config(None, sqlite_home, default_provider).await
}

/// Variant that also honours an explicit config-backed storage URL.
pub async fn open_or_create_runtime_db_with_config(
    storage_url: Option<&str>,
    sqlite_home: &Path,
    default_provider: &str,
) -> anyhow::Result<RuntimeDbHandle> {
    let vfs = mount_vfs_for(storage_url, sqlite_home).await?;
    Ok(runtime_handle_from_vfs(
        vfs,
        sqlite_home.to_path_buf(),
        default_provider.to_string(),
    ))
}

/// Get the runtime DB handle when a backend is mounted.
pub fn get_runtime_db(config: &Config) -> Option<RuntimeDbHandle> {
    get_runtime_db_for(config.sqlite_home.as_path(), &config.model_provider_id)
}

/// Trait-friendly variant: accepts only the fields needed to open the runtime DB.
pub fn get_runtime_db_for(sqlite_home: &Path, model_provider_id: &str) -> Option<RuntimeDbHandle> {
    let vfs = chaos_vfs::root_for(sqlite_home).ok()?;
    Some(runtime_handle_from_vfs(
        vfs,
        sqlite_home.to_path_buf(),
        model_provider_id.to_string(),
    ))
}

/// Open the runtime DB when a backend is mounted, without feature gating.
pub fn open_if_present(chaos_home: &Path, default_provider: &str) -> Option<RuntimeDbHandle> {
    get_runtime_db_for(chaos_home, default_provider)
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
) -> bool {
    let Some(ctx) = context else {
        return false;
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
                return false;
            }
        },
    };
    builder.cwd = normalize_cwd_for_runtime_db(&builder.cwd);
    match with_runtime_storage_breaker(|| {
        ctx.apply_rollout_items(
            &builder,
            items,
            new_process_memory_mode,
            updated_at_override,
        )
    })
    .await
    {
        Ok(()) => true,
        Err(BreakerError::Open) => false,
        Err(BreakerError::Operation(err)) => {
            warn!("runtime db apply_rollout_items failed during {stage}: {err}");
            false
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TouchProcessResult {
    Updated,
    Missing,
    Unavailable,
}

pub(crate) async fn touch_process_updated_at(
    context: Option<&RuntimeDbHandle>,
    process_id: Option<ProcessId>,
    updated_at: Timestamp,
    stage: &str,
) -> TouchProcessResult {
    let Some(ctx) = context else {
        return TouchProcessResult::Unavailable;
    };
    let Some(process_id) = process_id else {
        return TouchProcessResult::Missing;
    };
    match with_runtime_storage_breaker(|| ctx.touch_process_updated_at(process_id, updated_at))
        .await
    {
        Ok(true) => TouchProcessResult::Updated,
        Ok(false) => TouchProcessResult::Missing,
        Err(BreakerError::Open) => TouchProcessResult::Unavailable,
        Err(BreakerError::Operation(err)) => {
            warn!(
                "runtime db touch_process_updated_at failed during {stage} for {process_id}: {err}"
            );
            TouchProcessResult::Unavailable
        }
    }
}

#[cfg(test)]
#[path = "runtime_db_tests.rs"]
mod tests;
