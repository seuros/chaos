use chaos_ipc::ProcessId;
use chaos_ipc::protocol::{RolloutItem, SessionSource};
use sqlx::Row;

use crate::error::JournalError;
use crate::model::{ParentRef, ProcessRecord};
use crate::store::timestamp_from_epoch_seconds;

pub(super) fn serialize_source(source: &SessionSource) -> Result<String, JournalError> {
    serde_json::to_string(source).map_err(|source_err| JournalError::Serialize {
        field: "source_json",
        source: source_err,
    })
}

pub(super) fn serialize_item(item: &RolloutItem) -> Result<String, JournalError> {
    serde_json::to_string(item).map_err(|source| JournalError::Serialize {
        field: "payload_json",
        source,
    })
}

pub(super) fn deserialize_item(value: String) -> Result<RolloutItem, JournalError> {
    serde_json::from_str(&value).map_err(|source| JournalError::Deserialize {
        field: "payload_json",
        source,
    })
}

fn deserialize_source(value: String) -> Result<SessionSource, JournalError> {
    serde_json::from_str(&value).map_err(|source| JournalError::Deserialize {
        field: "source_json",
        source,
    })
}

pub(super) fn process_row_to_record(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ProcessRecord, JournalError> {
    let process_id_text: String = row.get("id");
    let process_id = ProcessId::from_string(&process_id_text).map_err(|source| {
        JournalError::InvalidProcessId {
            value: process_id_text,
            source,
        }
    })?;
    let parent = match row.get::<Option<String>, _>("parent_process_id") {
        Some(parent_process_id_text) => {
            let parent_process_id =
                ProcessId::from_string(&parent_process_id_text).map_err(|source| {
                    JournalError::InvalidProcessId {
                        value: parent_process_id_text,
                        source,
                    }
                })?;
            Some(ParentRef {
                parent_process_id,
                fork_at_seq: row.get("fork_at_seq"),
            })
        }
        None => None,
    };
    let source = deserialize_source(row.get("source_json"))?;
    let created_at = timestamp_from_epoch_seconds(row.get("created_at"))?;
    let updated_at = timestamp_from_epoch_seconds(row.get("updated_at"))?;
    let archived_at = row
        .get::<Option<i64>, _>("archived_at")
        .map(timestamp_from_epoch_seconds)
        .transpose()?;
    let cli_version: String = row.get("cli_version");
    Ok(ProcessRecord {
        process_id,
        parent,
        source,
        cwd: row.get::<String, _>("cwd").into(),
        created_at,
        updated_at,
        archived_at,
        title: row.get("title"),
        model_provider: row.get("model_provider"),
        cli_version: (!cli_version.is_empty()).then_some(cli_version),
        agent_nickname: row.get("agent_nickname"),
        agent_role: row.get("agent_role"),
    })
}
