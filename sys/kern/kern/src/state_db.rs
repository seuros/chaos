use crate::config::Config;
use crate::path_utils::normalize_for_path_comparison;
use crate::rollout::list::Cursor;
use crate::rollout::list::ProcessSortKey;
use crate::rollout::metadata;
use chaos_ipc::ProcessId;
use chaos_ipc::dynamic_tools::DynamicToolSpec;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionSource;
pub use chaos_proc::LogEntry;
use chaos_proc::ProcessMetadataBuilder;
use jiff::Timestamp;
use serde_json::Value;
use sqlx::SqlitePool;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

/// Core-facing handle to the SQLite-backed state runtime.
pub type StateDbHandle = Arc<chaos_proc::StateRuntime>;

/// Initialize the state runtime for thread state persistence. To only be used
/// inside `core`. The initialization should not be done anywhere else.
pub(crate) async fn init(config: &Config) -> Option<StateDbHandle> {
    let runtime = match chaos_proc::StateRuntime::init(
        config.sqlite_home.clone(),
        config.model_provider_id.clone(),
    )
    .await
    {
        Ok(runtime) => runtime,
        Err(err) => {
            warn!(
                "failed to initialize state runtime at {}: {err}",
                config.sqlite_home.display()
            );
            return None;
        }
    };

    // Spawn the process-wide cron scheduler if the chaos pool is available.
    if let Some(chaos_pool) = runtime.chaos_pool() {
        chaos_cron::spawn_scheduler(chaos_pool.to_owned(), chaos_cron::shell_executor());
    }

    Some(runtime)
}

/// Resolve the shared chaos.sqlite pool, opening it lazily when the state runtime
/// is unavailable.
///
/// Cron jobs live in the main chaos DB rather than the per-session runtime, so
/// callers should use this even for sessions that are otherwise ephemeral.
pub async fn resolve_chaos_pool(
    existing_pool: Option<SqlitePool>,
    sqlite_home: &Path,
) -> Option<SqlitePool> {
    if let Some(pool) = existing_pool {
        return Some(pool);
    }

    match chaos_proc::open_chaos_db(sqlite_home).await {
        Ok(pool) => Some(pool),
        Err(err) => {
            warn!(
                "failed to open chaos db on demand at {}: {err}",
                chaos_proc::chaos_db_path(sqlite_home).display()
            );
            None
        }
    }
}

/// Get the DB if the feature is enabled and the DB exists.
pub async fn get_state_db(config: &Config) -> Option<StateDbHandle> {
    get_state_db_for(config.sqlite_home.as_path(), &config.model_provider_id).await
}

/// Trait-friendly variant: accepts only the fields needed to open the state DB.
pub async fn get_state_db_for(
    sqlite_home: &Path,
    model_provider_id: &str,
) -> Option<StateDbHandle> {
    let state_path = chaos_proc::state_db_path(sqlite_home);
    if !tokio::fs::try_exists(&state_path).await.unwrap_or(false) {
        return None;
    }
    let runtime =
        chaos_proc::StateRuntime::init(sqlite_home.to_path_buf(), model_provider_id.to_string())
            .await
            .ok()?;
    Some(runtime)
}

/// Open the state runtime when the SQLite file exists, without feature gating.
///
/// This is used for parity checks during the SQLite migration phase.
pub async fn open_if_present(chaos_home: &Path, default_provider: &str) -> Option<StateDbHandle> {
    let db_path = chaos_proc::state_db_path(chaos_home);
    if !tokio::fs::try_exists(&db_path).await.unwrap_or(false) {
        return None;
    }
    let runtime =
        chaos_proc::StateRuntime::init(chaos_home.to_path_buf(), default_provider.to_string())
            .await
            .ok()?;
    Some(runtime)
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

/// Parse a `YYYY-MM-DDThh-mm-ss` filename timestamp into a `jiff::Timestamp`.
fn parse_filename_timestamp(ts_str: &str) -> Option<Timestamp> {
    if ts_str.len() < 19 {
        return None;
    }
    // "2026-01-27T12-34-56" → "2026-01-27T12:34:56Z"
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

pub(crate) fn normalize_cwd_for_state_db(cwd: &Path) -> PathBuf {
    normalize_for_path_comparison(cwd).unwrap_or_else(|_| cwd.to_path_buf())
}

/// List thread ids from SQLite for parity checks without rollout scanning.
#[allow(clippy::too_many_arguments)]
pub async fn list_process_ids_db(
    context: Option<&chaos_proc::StateRuntime>,
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
            "state db chaos_home mismatch: expected {}, got {}",
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
            warn!("state db list_process_ids failed during {stage}: {err}");
            None
        }
    }
}

/// List process metadata from SQLite without rollout directory traversal.
#[allow(clippy::too_many_arguments)]
pub async fn list_processes_db(
    context: Option<&chaos_proc::StateRuntime>,
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
            "state db chaos_home mismatch: expected {}, got {}",
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
            warn!("state db list_processes failed: {err}");
            None
        }
    }
}

/// Get dynamic tools for a thread id using SQLite.
pub async fn get_dynamic_tools(
    context: Option<&chaos_proc::StateRuntime>,
    process_id: ProcessId,
    stage: &str,
) -> Option<Vec<DynamicToolSpec>> {
    let ctx = context?;
    match ctx.get_dynamic_tools(process_id).await {
        Ok(tools) => tools,
        Err(err) => {
            warn!("state db get_dynamic_tools failed during {stage}: {err}");
            None
        }
    }
}

/// Persist dynamic tools for a thread id using SQLite, if none exist yet.
pub async fn persist_dynamic_tools(
    context: Option<&chaos_proc::StateRuntime>,
    process_id: ProcessId,
    tools: Option<&[DynamicToolSpec]>,
    stage: &str,
) {
    let Some(ctx) = context else {
        return;
    };
    if let Err(err) = ctx.persist_dynamic_tools(process_id, tools).await {
        warn!("state db persist_dynamic_tools failed during {stage}: {err}");
    }
}

pub async fn mark_process_memory_mode_polluted(
    context: Option<&chaos_proc::StateRuntime>,
    process_id: ProcessId,
    stage: &str,
) {
    let Some(ctx) = context else {
        return;
    };
    if let Err(err) = ctx.mark_process_memory_mode_polluted(process_id).await {
        warn!("state db mark_process_memory_mode_polluted failed during {stage}: {err}");
    }
}

/// Apply persisted session items incrementally to SQLite.
#[allow(clippy::too_many_arguments)]
pub async fn apply_rollout_items(
    context: Option<&chaos_proc::StateRuntime>,
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
                warn!("state db apply_rollout_items missing builder during {stage}");
                warn!("state db discrepancy during apply_rollout_items: {stage}, missing_builder");
                return;
            }
        },
    };
    builder.cwd = normalize_cwd_for_state_db(&builder.cwd);
    if let Err(err) = ctx
        .apply_rollout_items(
            &builder,
            items,
            new_process_memory_mode,
            updated_at_override,
        )
        .await
    {
        warn!("state db apply_rollout_items failed during {stage}: {err}");
    }
}

pub async fn touch_process_updated_at(
    context: Option<&chaos_proc::StateRuntime>,
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
                "state db touch_process_updated_at failed during {stage} for {process_id}: {err}"
            );
            false
        })
}

#[cfg(test)]
#[path = "state_db_tests.rs"]
mod tests;
