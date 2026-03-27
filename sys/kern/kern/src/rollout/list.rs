//! Thread listing — thin shim over `chaos_rollout::list`.
//!
//! Pure filesystem scanning, parsing, and pagination live in the `chaos-rollout`
//! crate. This module re-exports those types and adds the state_db-dependent
//! lookup functions that require codex-core infrastructure.

use std::io;
use std::num::NonZero;
use std::path::Path;
use std::path::PathBuf;

use chaos_locate as file_search;
use chaos_ipc::ProcessId;
use time::OffsetDateTime;

use super::ARCHIVED_SESSIONS_SUBDIR;
use super::SESSIONS_SUBDIR;
use crate::state_db;

// Re-export the shared process-listing types and helpers from chaos-rollout.
pub use chaos_rollout::list::Cursor;
pub use chaos_rollout::list::ProcessItem;
pub use chaos_rollout::list::ProcessListConfig;
pub use chaos_rollout::list::ProcessListLayout;
pub use chaos_rollout::list::ProcessSortKey;
pub use chaos_rollout::list::ProcessesPage;
pub use chaos_rollout::list::get_processes;
pub use chaos_rollout::list::get_processes_in_root;
pub use chaos_rollout::list::parse_cursor;
pub use chaos_rollout::list::parse_timestamp_uuid_from_filename;
pub use chaos_rollout::list::read_head_for_summary;
pub use chaos_rollout::list::read_session_meta_line;
pub use chaos_rollout::list::rollout_date_parts;

/// Bridge: convert a `chaos_proc::Anchor` into a `Cursor`.
///
/// This cannot be a `From` impl due to the orphan rule (both types are
/// foreign to codex-core).
pub fn cursor_from_anchor(anchor: chaos_proc::Anchor) -> Cursor {
    let ts = OffsetDateTime::from_unix_timestamp(anchor.ts.as_second())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    Cursor::new(ts, anchor.id)
}

// ---------------------------------------------------------------------------
// state_db-dependent lookup functions (cannot move to chaos-rollout)
// ---------------------------------------------------------------------------

async fn find_process_path_by_id_str_in_subdir(
    codex_home: &Path,
    subdir: &str,
    id_str: &str,
) -> io::Result<Option<PathBuf>> {
    // Validate UUID format early.
    if uuid::Uuid::parse_str(id_str).is_err() {
        return Ok(None);
    }

    // Prefer DB lookup, then fall back to rollout file search.
    // TODO(jif): sqlite migration phase 1
    let archived_only = match subdir {
        SESSIONS_SUBDIR => Some(false),
        ARCHIVED_SESSIONS_SUBDIR => Some(true),
        _ => None,
    };
    let process_id = ProcessId::from_string(id_str).ok();
    let state_db_ctx = state_db::open_if_present(codex_home, "").await;
    if let Some(state_db_ctx) = state_db_ctx.as_deref()
        && let Some(process_id) = process_id
        && let Some(db_path) = state_db::find_rollout_path_by_id(
            Some(state_db_ctx),
            process_id,
            archived_only,
            "find_path_query",
        )
        .await
    {
        if tokio::fs::try_exists(&db_path).await.unwrap_or(false) {
            return Ok(Some(db_path));
        }
        tracing::error!(
            "state db returned stale rollout path for process {id_str}: {}",
            db_path.display()
        );
        tracing::warn!(
            "state db discrepancy during find_process_path_by_id_str_in_subdir: stale_db_path"
        );
    }

    let mut root = codex_home.to_path_buf();
    root.push(subdir);
    if !root.exists() {
        return Ok(None);
    }
    // This is safe because we know the values are valid.
    #[allow(clippy::unwrap_used)]
    let limit = NonZero::new(1).unwrap();
    let options = file_search::FileSearchOptions {
        limit,
        compute_indices: false,
        respect_gitignore: false,
        ..Default::default()
    };

    let results = file_search::run(id_str, vec![root], options, /*cancel_flag*/ None)
        .map_err(|e| io::Error::other(format!("file search failed: {e}")))?;

    let found = results.matches.into_iter().next().map(|m| m.full_path());
    if let Some(found_path) = found.as_ref() {
        tracing::debug!("state db missing rollout path for process {id_str}");
        tracing::warn!(
            "state db discrepancy during find_process_path_by_id_str_in_subdir: falling_back"
        );
        state_db::read_repair_rollout_path(
            state_db_ctx.as_deref(),
            process_id,
            archived_only,
            found_path.as_path(),
        )
        .await;
    }

    Ok(found)
}

/// Locate a recorded process rollout file by its UUID string using the existing
/// paginated listing implementation. Returns `Ok(Some(path))` if found, `Ok(None)` if not present
/// or the id is invalid.
pub async fn find_process_path_by_id_str(
    codex_home: &Path,
    id_str: &str,
) -> io::Result<Option<PathBuf>> {
    find_process_path_by_id_str_in_subdir(codex_home, SESSIONS_SUBDIR, id_str).await
}

/// Locate an archived process rollout file by its UUID string.
pub async fn find_archived_process_path_by_id_str(
    codex_home: &Path,
    id_str: &str,
) -> io::Result<Option<PathBuf>> {
    find_process_path_by_id_str_in_subdir(codex_home, ARCHIVED_SESSIONS_SUBDIR, id_str).await
}
