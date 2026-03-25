pub use chaos_rollout::session_index::*;

use std::path::Path;
use std::path::PathBuf;

/// Locate a recorded thread rollout file by thread name using newest-first ordering.
/// Returns `Ok(Some(path))` if found, `Ok(None)` if not present.
pub async fn find_thread_path_by_name_str(
    codex_home: &Path,
    name: &str,
) -> std::io::Result<Option<PathBuf>> {
    let Some(thread_id) = find_thread_id_by_name(codex_home, name).await? else {
        return Ok(None);
    };
    super::list::find_thread_path_by_id_str(codex_home, &thread_id.to_string()).await
}

#[cfg(test)]
#[path = "session_index_tests.rs"]
mod tests;
