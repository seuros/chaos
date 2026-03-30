use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use chaos_ipc::ProcessId;

async fn open_state_db(
    chaos_home: &Path,
) -> std::io::Result<Option<Arc<chaos_proc::StateRuntime>>> {
    let db_path = chaos_proc::state_db_path(chaos_home);
    if !tokio::fs::try_exists(&db_path).await? {
        return Ok(None);
    }
    Ok(Some(
        chaos_proc::StateRuntime::init(chaos_home.to_path_buf(), "journal".to_string())
            .await
            .map_err(std::io::Error::other)?,
    ))
}

/// Persist the explicit process name in SQLite.
pub async fn append_process_name(
    chaos_home: &Path,
    process_id: ProcessId,
    name: &str,
) -> std::io::Result<()> {
    let Some(runtime) = open_state_db(chaos_home).await? else {
        return Err(std::io::Error::other(
            "state db is unavailable; cannot persist process name",
        ));
    };
    let updated = runtime
        .set_process_name(process_id, Some(name))
        .await
        .map_err(std::io::Error::other)?;
    if updated {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "process not found while persisting process name",
        ))
    }
}

/// Find the explicit process name for a process id, if any.
pub async fn find_process_name_by_id(
    chaos_home: &Path,
    process_id: &ProcessId,
) -> std::io::Result<Option<String>> {
    let Some(runtime) = open_state_db(chaos_home).await? else {
        return Ok(None);
    };
    runtime
        .get_process_name(*process_id)
        .await
        .map_err(std::io::Error::other)
}

/// Find explicit process names for a batch of process ids.
pub async fn find_process_names_by_ids(
    chaos_home: &Path,
    process_ids: &HashSet<ProcessId>,
) -> std::io::Result<HashMap<ProcessId, String>> {
    let Some(runtime) = open_state_db(chaos_home).await? else {
        return Ok(HashMap::new());
    };
    runtime
        .get_process_names(process_ids)
        .await
        .map_err(std::io::Error::other)
}

/// Find the most recently updated process id for a process name, if any.
pub async fn find_process_id_by_name(
    chaos_home: &Path,
    name: &str,
) -> std::io::Result<Option<ProcessId>> {
    let Some(runtime) = open_state_db(chaos_home).await? else {
        return Ok(None);
    };
    runtime
        .find_process_id_by_name(name)
        .await
        .map_err(std::io::Error::other)
}
