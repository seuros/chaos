use std::future::Future;
use std::time::Duration;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::{RolloutItem, SessionSource};

use crate::error::JournalError;
use crate::model::AppendBatchInput;
use crate::model::AppendBatchResult;
use crate::model::CreateProcessInput;
use crate::model::InitializeProcessInput;
use crate::model::InitializeProcessResult;
use crate::model::JournalEntry;
use crate::model::Lease;
use crate::model::LoadedJournal;
use crate::model::OwnerId;
use crate::model::ProcessRecord;

pub trait JournalStore {
    fn archive_idle_process(
        &self,
        request: crate::ArchiveIdleProcessRequest,
    ) -> impl Future<Output = Result<(), JournalError>> + Send;

    /// Transactional control-plane identity, idempotency and fenced ownership.
    fn execution(
        &self,
        request: crate::ExecutionRequest,
    ) -> impl Future<Output = Result<crate::ExecutionResponse, JournalError>> + Send;

    fn create_process(
        &self,
        input: CreateProcessInput,
    ) -> impl Future<Output = Result<ProcessRecord, JournalError>> + Send;

    /// Atomic process creation + first-batch append + lease acquisition.
    ///
    /// All three steps run inside one storage transaction. On any failure the entire
    /// transaction rolls back so readers never observe a process row without its
    /// first journal entries.
    fn initialize_process(
        &self,
        input: InitializeProcessInput,
    ) -> impl Future<Output = Result<InitializeProcessResult, JournalError>> + Send;

    fn get_process(
        &self,
        process_id: &ProcessId,
    ) -> impl Future<Output = Result<Option<ProcessRecord>, JournalError>> + Send;

    fn list_processes(
        &self,
        archived: Option<bool>,
    ) -> impl Future<Output = Result<Vec<ProcessRecord>, JournalError>> + Send;

    fn acquire_lease(
        &self,
        process_id: &ProcessId,
        owner_id: &OwnerId,
        ttl: Duration,
    ) -> impl Future<Output = Result<Lease, JournalError>> + Send;

    fn heartbeat_lease(
        &self,
        process_id: &ProcessId,
        owner_id: &OwnerId,
        lease_token: &str,
        ttl: Duration,
    ) -> impl Future<Output = Result<Lease, JournalError>> + Send;

    fn release_lease(
        &self,
        process_id: &ProcessId,
        owner_id: &OwnerId,
        lease_token: &str,
    ) -> impl Future<Output = Result<(), JournalError>> + Send;

    fn append_batch(
        &self,
        input: AppendBatchInput,
    ) -> impl Future<Output = Result<AppendBatchResult, JournalError>> + Send;

    fn load_journal(
        &self,
        process_id: &ProcessId,
    ) -> impl Future<Output = Result<LoadedJournal, JournalError>> + Send;

    fn get_default_process(
        &self,
    ) -> impl Future<Output = Result<Option<ProcessId>, JournalError>> + Send;

    fn set_default_process(
        &self,
        process_id: &ProcessId,
    ) -> impl Future<Output = Result<(), JournalError>> + Send;
}

/// Apply the same metadata defaults for both backends and creation paths.
pub(crate) fn new_process_record(
    input: CreateProcessInput,
    updated_at: jiff::Timestamp,
) -> ProcessRecord {
    let agent_nickname = input.source.get_nickname();
    let agent_role = input.source.get_agent_role();
    ProcessRecord {
        process_id: input.process_id,
        parent: input.parent,
        source: input.source,
        cwd: input.cwd,
        created_at: input.created_at,
        updated_at,
        archived_at: None,
        title: input.title.unwrap_or_default(),
        model_provider: input
            .model_provider
            .unwrap_or_else(|| "unknown".to_string()),
        cli_version: input.cli_version,
        agent_nickname,
        agent_role,
    }
}

pub(crate) fn process_insert_error(process_id: ProcessId, err: sqlx::Error) -> JournalError {
    match err {
        sqlx::Error::Database(db_err) if db_err.is_unique_violation() => {
            JournalError::ProcessAlreadyExists(process_id)
        }
        other => JournalError::Db(other),
    }
}

pub(crate) fn validate_sequences(
    process_id: ProcessId,
    first_seq: i64,
    items: &[JournalEntry],
) -> Result<(), JournalError> {
    for (index, entry) in items.iter().enumerate() {
        let expected = first_seq + index as i64;
        if entry.seq != expected {
            return Err(JournalError::SequenceConflict {
                process_id,
                expected_next_seq: expected,
                actual_next_seq: entry.seq,
            });
        }
    }
    Ok(())
}

pub(crate) fn source_label(source: &SessionSource) -> String {
    match serde_json::to_value(source) {
        Ok(serde_json::Value::String(label)) => label,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

pub(crate) fn journal_item_type(item: &RolloutItem) -> &'static str {
    match item {
        RolloutItem::SessionMeta(_) => "session_meta",
        RolloutItem::ResponseItem(_) => "response_item",
        RolloutItem::Compacted(_) => "compacted",
        RolloutItem::CompactionControl(_) => "compaction_control",
        RolloutItem::TurnContext(_) => "turn_context",
        RolloutItem::EventMsg(_) => "event_msg",
        RolloutItem::BackgroundTask(_) => "background_task",
        RolloutItem::GoalCheckpoint(_) => "goal_checkpoint",
        RolloutItem::ObservedModelContextWindow(_) => "observed_model_context_window",
    }
}

pub(crate) fn timestamp_after(
    base: jiff::Timestamp,
    ttl: Duration,
) -> Result<jiff::Timestamp, JournalError> {
    let ttl_seconds = i64::try_from(ttl.as_secs()).map_err(|_| JournalError::InvalidTimestamp {
        value: ttl.as_secs().to_string(),
        message: "ttl seconds overflow i64".to_string(),
    })?;
    let expires = base.as_second().checked_add(ttl_seconds).ok_or_else(|| {
        JournalError::InvalidTimestamp {
            value: ttl.as_secs().to_string(),
            message: "timestamp overflow".to_string(),
        }
    })?;
    timestamp_from_epoch_seconds(expires)
}

pub(crate) fn timestamp_from_epoch_seconds(seconds: i64) -> Result<jiff::Timestamp, JournalError> {
    jiff::Timestamp::from_second(seconds).map_err(|err| JournalError::InvalidTimestamp {
        value: seconds.to_string(),
        message: err.to_string(),
    })
}

#[cfg(test)]
mod tests;
