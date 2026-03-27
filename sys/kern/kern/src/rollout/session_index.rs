pub use chaos_rollout::session_index::*;

use std::path::Path;
use std::path::PathBuf;

pub async fn find_process_path_by_name_str(
    codex_home: &Path,
    name: &str,
) -> std::io::Result<Option<PathBuf>> {
    let Some(process_id) = find_process_id_by_name(codex_home, name).await? else {
        return Ok(None);
    };
    super::list::find_process_path_by_id_str(codex_home, &process_id.to_string()).await
}

#[cfg(test)]
#[path = "session_index_tests.rs"]
mod tests;
