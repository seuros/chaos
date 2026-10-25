use chaos_ipc::ProcessId;
use chaos_ipc::protocol::{RolloutItem, SessionSource};
use serde_json::Value;
use sqlx::Row;
use sqlx::postgres::PgRow;

use crate::error::JournalError;
use crate::model::{ParentRef, ProcessRecord};
use crate::store::timestamp_from_epoch_seconds;

pub(super) fn process_row_to_record(row: &PgRow) -> Result<ProcessRecord, JournalError> {
    let process_id_text: String = row.get("id");
    let process_id = parse_process_id(process_id_text)?;
    let parent = match row.get::<Option<String>, _>("parent_process_id") {
        Some(parent_process_id_text) => Some(ParentRef {
            parent_process_id: parse_process_id(parent_process_id_text)?,
            fork_at_seq: row.get("fork_at_seq"),
        }),
        None => None,
    };
    let source = deserialize_source(row.get("source_json"))?;
    let cli_version: String = row.get("cli_version");
    Ok(ProcessRecord {
        process_id,
        parent,
        source,
        cwd: row.get::<String, _>("cwd").into(),
        created_at: timestamp_from_epoch_seconds(row.get("created_at"))?,
        updated_at: timestamp_from_epoch_seconds(row.get("updated_at"))?,
        archived_at: row
            .get::<Option<i64>, _>("archived_at")
            .map(timestamp_from_epoch_seconds)
            .transpose()?,
        title: row.get("title"),
        model_provider: row.get("model_provider"),
        cli_version: (!cli_version.is_empty()).then_some(cli_version),
        agent_nickname: row.get("agent_nickname"),
        agent_role: row.get("agent_role"),
    })
}

fn parse_process_id(value: String) -> Result<ProcessId, JournalError> {
    ProcessId::from_string(&value)
        .map_err(|source| JournalError::InvalidProcessId { value, source })
}

pub(super) fn serialize_source(source: &SessionSource) -> Result<Value, JournalError> {
    serde_json::to_value(source)
        .map(jsonb_safe)
        .map_err(|source| JournalError::Serialize {
            field: "source_json",
            source,
        })
}

fn deserialize_source(value: Value) -> Result<SessionSource, JournalError> {
    serde_json::from_value(value).map_err(|source| JournalError::Deserialize {
        field: "source_json",
        source,
    })
}

pub(super) fn serialize_item(item: &RolloutItem) -> Result<Value, JournalError> {
    serde_json::to_value(item)
        .map(jsonb_safe)
        .map_err(|source| JournalError::Serialize {
            field: "payload_json",
            source,
        })
}

/// PostgreSQL `jsonb` cannot store U+0000; it is stored as U+FFFD instead.
pub(super) fn jsonb_safe(mut value: Value) -> Value {
    replace_nul(&mut value);
    value
}

fn replace_nul(value: &mut Value) {
    match value {
        Value::String(text) => replace_nul_in(text),
        Value::Array(items) => items.iter_mut().for_each(replace_nul),
        Value::Object(map) => {
            if map.keys().any(|key| key.contains('\0')) {
                *map = std::mem::take(map)
                    .into_iter()
                    .map(|(mut key, value)| {
                        replace_nul_in(&mut key);
                        (key, value)
                    })
                    .collect();
            }
            map.values_mut().for_each(replace_nul);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn replace_nul_in(text: &mut String) {
    if text.contains('\0') {
        *text = text.replace('\0', "\u{FFFD}");
    }
}

pub(super) fn deserialize_item(value: Value) -> Result<RolloutItem, JournalError> {
    serde_json::from_value(value).map_err(|source| JournalError::Deserialize {
        field: "payload_json",
        source,
    })
}
