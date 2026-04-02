use std::path::PathBuf;

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionSource;
use serde::Deserialize;
use serde::Serialize;

pub type OwnerId = String;
pub type LeaseToken = String;
pub type EntrySeq = i64;

pub const SQLITE_DB_FILENAME: &str = "journal_1.sqlite";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentRef {
    pub parent_process_id: ProcessId,
    pub fork_at_seq: EntrySeq,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub seq: EntrySeq,
    pub recorded_at: jiff::Timestamp,
    pub item: RolloutItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRecord {
    pub process_id: ProcessId,
    pub parent: Option<ParentRef>,
    pub source: SessionSource,
    pub cwd: PathBuf,
    pub created_at: jiff::Timestamp,
    pub updated_at: jiff::Timestamp,
    pub archived_at: Option<jiff::Timestamp>,
    pub title: String,
    pub model_provider: String,
    pub cli_version: Option<String>,
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateProcessInput {
    pub process_id: ProcessId,
    pub parent: Option<ParentRef>,
    pub source: SessionSource,
    pub cwd: PathBuf,
    pub created_at: jiff::Timestamp,
    pub title: Option<String>,
    pub model_provider: Option<String>,
    pub cli_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub process_id: ProcessId,
    pub owner_id: OwnerId,
    pub lease_token: LeaseToken,
    pub expires_at: jiff::Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppendBatchInput {
    pub process_id: ProcessId,
    pub owner_id: OwnerId,
    pub lease_token: LeaseToken,
    pub expected_next_seq: EntrySeq,
    pub items: Vec<JournalEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendBatchResult {
    pub next_seq: EntrySeq,
    pub updated_at: jiff::Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedJournal {
    pub process_id: ProcessId,
    pub parent: Option<ParentRef>,
    pub items: Vec<JournalEntry>,
    pub next_seq: EntrySeq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloRequest {
    pub client_name: String,
    pub protocol_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResponse {
    pub server_name: String,
    pub protocol_version: u32,
    pub backend: String,
}
