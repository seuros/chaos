//! Persistence layer for the global, append-only message-history store.
//!
//! Entries are kept in the runtime DB so the TUI composer can recall prior
//! user prompts across sessions without relying on a separate JSONL sidecar.

use std::io::Result;

use crate::config::Config;
use crate::config::types::HistoryPersistence;
use crate::runtime_db::RuntimeDbHandle;

use chaos_ipc::ProcessId;
pub use chaos_ipc::message_history::HistoryEntry;
use tracing::warn;

pub(crate) async fn append_entry(
    text: &str,
    conversation_id: &ProcessId,
    runtime_db: Option<&RuntimeDbHandle>,
    config: &Config,
) -> Result<()> {
    match config.history.persistence {
        HistoryPersistence::SaveAll => {}
        HistoryPersistence::None => return Ok(()),
    }

    let Some(runtime_db) = runtime_db else {
        return Ok(());
    };

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| std::io::Error::other(format!("system clock before Unix epoch: {e}")))?
        .as_secs();

    let entry = HistoryEntry {
        conversation_id: conversation_id.to_string(),
        ts,
        text: text.to_string(),
    };

    runtime_db
        .append_message_history_entry(&entry, config.history.max_bytes)
        .await
        .map_err(std::io::Error::other)
}

/// Fetch the current store identifier and number of persisted entries.
pub(crate) async fn history_metadata(runtime_db: Option<&RuntimeDbHandle>) -> (u64, usize) {
    let Some(runtime_db) = runtime_db else {
        return (0, 0);
    };
    match runtime_db.message_history_metadata().await {
        Ok(metadata) => metadata,
        Err(err) => {
            warn!("failed to read message history metadata: {err}");
            (0, 0)
        }
    }
}

/// Look up a single persistent history entry by offset.
pub(crate) async fn lookup(
    log_id: u64,
    offset: usize,
    runtime_db: Option<&RuntimeDbHandle>,
) -> Option<HistoryEntry> {
    let runtime_db = runtime_db?;
    match runtime_db.get_message_history_entry(log_id, offset).await {
        Ok(entry) => entry,
        Err(err) => {
            warn!("failed to read message history entry: {err}");
            None
        }
    }
}

#[cfg(test)]
#[path = "message_history_tests.rs"]
mod tests;
