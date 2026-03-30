use std::future::Future;
use std::time::Duration;

use chaos_ipc::ProcessId;

use crate::error::JournalError;
use crate::model::AppendBatchInput;
use crate::model::AppendBatchResult;
use crate::model::CreateProcessInput;
use crate::model::Lease;
use crate::model::LoadedJournal;
use crate::model::OwnerId;
use crate::model::ProcessRecord;

pub trait JournalStore {
    fn create_process(
        &self,
        input: CreateProcessInput,
    ) -> impl Future<Output = Result<ProcessRecord, JournalError>> + Send;

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
}
