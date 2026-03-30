use chaos_ipc::ProcessId;
use serde::{Deserialize, Serialize};

use crate::model::AppendBatchInput;
use crate::model::AppendBatchResult;
use crate::model::CreateProcessInput;
use crate::model::HelloRequest;
use crate::model::HelloResponse;
use crate::model::Lease;
use crate::model::LoadedJournal;
use crate::model::OwnerId;
use crate::model::ProcessRecord;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub id: String,
    #[serde(flatten)]
    pub request: JournalRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum JournalRequest {
    Hello(HelloRequest),
    CreateProcess(CreateProcessInput),
    GetProcess(GetProcessRequest),
    ListProcesses(ListProcessesRequest),
    AcquireLease(AcquireLeaseRequest),
    HeartbeatLease(HeartbeatLeaseRequest),
    ReleaseLease(ReleaseLeaseRequest),
    AppendBatch(AppendBatchInput),
    LoadJournal(LoadJournalRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<JournalResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "result", rename_all = "snake_case")]
pub enum JournalResponse {
    Hello(HelloResponse),
    CreateProcess(CreateProcessResponse),
    GetProcess(Box<GetProcessResponse>),
    ListProcesses(ListProcessesResponse),
    AcquireLease(Lease),
    HeartbeatLease(Lease),
    ReleaseLease(ReleaseLeaseResponse),
    AppendBatch(AppendBatchResult),
    LoadJournal(LoadedJournal),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcquireLeaseRequest {
    pub process_id: ProcessId,
    pub owner_id: OwnerId,
    pub ttl_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetProcessRequest {
    pub process_id: ProcessId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListProcessesRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatLeaseRequest {
    pub process_id: ProcessId,
    pub owner_id: OwnerId,
    pub lease_token: String,
    pub ttl_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseLeaseRequest {
    pub process_id: ProcessId,
    pub owner_id: OwnerId,
    pub lease_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadJournalRequest {
    pub process_id: ProcessId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateProcessResponse {
    pub process_id: ProcessId,
    pub next_seq: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetProcessResponse {
    pub process: Option<ProcessRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListProcessesResponse {
    pub items: Vec<ProcessRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseLeaseResponse {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    AlreadyExists,
    LeaseConflict,
    LeaseExpired,
    InvalidLease,
    SequenceConflict,
    Internal,
}
