use std::time::Duration;

use chaos_ipc::ProcessId;
use sqlx::PgPool;

use crate::AppendBatchInput;
use crate::AppendBatchResult;
use crate::CreateProcessInput;
use crate::CreateProcessResponse;
use crate::InitializeProcessInput;
use crate::InitializeProcessResult;
use crate::JournalClientError;
use crate::JournalRpcClient;
use crate::JournalStore;
use crate::Lease;
use crate::LoadedJournal;
use crate::PostgresJournalStore;
use crate::ProcessRecord;
use crate::rama_http::error_payload_for;

/// Backend-neutral journal access used by the kernel.
///
/// SQLite keeps the journald Unix-socket sidecar. PostgreSQL uses the mounted
/// pool directly and never starts or contacts journald.
#[derive(Debug, Clone)]
pub enum JournalClient {
    Rpc(JournalRpcClient),
    Postgres(PostgresJournalStore),
}

impl JournalClient {
    pub fn rpc(client: JournalRpcClient) -> Self {
        Self::Rpc(client)
    }

    pub fn postgres(store: PostgresJournalStore) -> Self {
        Self::Postgres(store)
    }

    pub fn postgres_pool(pool: PgPool) -> Self {
        Self::postgres(PostgresJournalStore::new(pool))
    }

    pub async fn create_process(
        &self,
        input: CreateProcessInput,
    ) -> Result<CreateProcessResponse, JournalClientError> {
        match self {
            Self::Rpc(client) => client.create_process(input).await,
            Self::Postgres(store) => {
                let process = store.create_process(input).await.map_err(store_error)?;
                Ok(CreateProcessResponse {
                    process_id: process.process_id,
                    next_seq: 0,
                })
            }
        }
    }

    pub async fn initialize_process(
        &self,
        input: InitializeProcessInput,
    ) -> Result<InitializeProcessResult, JournalClientError> {
        match self {
            Self::Rpc(client) => client.initialize_process(input).await,
            Self::Postgres(store) => store.initialize_process(input).await.map_err(store_error),
        }
    }

    pub async fn get_process(
        &self,
        process_id: ProcessId,
    ) -> Result<Option<ProcessRecord>, JournalClientError> {
        match self {
            Self::Rpc(client) => client.get_process(process_id).await,
            Self::Postgres(store) => store.get_process(&process_id).await.map_err(store_error),
        }
    }

    pub async fn list_processes(
        &self,
        archived: Option<bool>,
    ) -> Result<Vec<ProcessRecord>, JournalClientError> {
        match self {
            Self::Rpc(client) => client.list_processes(archived).await,
            Self::Postgres(store) => store.list_processes(archived).await.map_err(store_error),
        }
    }

    pub async fn acquire_lease(
        &self,
        process_id: ProcessId,
        owner_id: String,
        ttl_ms: u64,
    ) -> Result<Lease, JournalClientError> {
        match self {
            Self::Rpc(client) => client.acquire_lease(process_id, owner_id, ttl_ms).await,
            Self::Postgres(store) => store
                .acquire_lease(&process_id, &owner_id, Duration::from_millis(ttl_ms))
                .await
                .map_err(store_error),
        }
    }

    pub async fn heartbeat_lease(
        &self,
        process_id: ProcessId,
        owner_id: String,
        lease_token: String,
        ttl_ms: u64,
    ) -> Result<Lease, JournalClientError> {
        match self {
            Self::Rpc(client) => {
                client
                    .heartbeat_lease(process_id, owner_id, lease_token, ttl_ms)
                    .await
            }
            Self::Postgres(store) => store
                .heartbeat_lease(
                    &process_id,
                    &owner_id,
                    &lease_token,
                    Duration::from_millis(ttl_ms),
                )
                .await
                .map_err(store_error),
        }
    }

    pub async fn release_lease(
        &self,
        process_id: ProcessId,
        owner_id: String,
        lease_token: String,
    ) -> Result<(), JournalClientError> {
        match self {
            Self::Rpc(client) => {
                client
                    .release_lease(process_id, owner_id, lease_token)
                    .await
            }
            Self::Postgres(store) => store
                .release_lease(&process_id, &owner_id, &lease_token)
                .await
                .map_err(store_error),
        }
    }

    pub async fn append_batch(
        &self,
        input: AppendBatchInput,
    ) -> Result<AppendBatchResult, JournalClientError> {
        match self {
            Self::Rpc(client) => client.append_batch(input).await,
            Self::Postgres(store) => store.append_batch(input).await.map_err(store_error),
        }
    }

    pub async fn load_journal(
        &self,
        process_id: ProcessId,
    ) -> Result<LoadedJournal, JournalClientError> {
        match self {
            Self::Rpc(client) => client.load_journal(process_id).await,
            Self::Postgres(store) => store.load_journal(&process_id).await.map_err(store_error),
        }
    }

    pub async fn get_default_process(&self) -> Result<Option<ProcessId>, JournalClientError> {
        match self {
            Self::Rpc(client) => client.get_default_process().await,
            Self::Postgres(store) => store.get_default_process().await.map_err(store_error),
        }
    }

    pub async fn set_default_process(
        &self,
        process_id: ProcessId,
    ) -> Result<(), JournalClientError> {
        match self {
            Self::Rpc(client) => client.set_default_process(process_id).await,
            Self::Postgres(store) => store
                .set_default_process(&process_id)
                .await
                .map_err(store_error),
        }
    }
}

fn store_error(error: crate::JournalError) -> JournalClientError {
    JournalClientError::Remote(error_payload_for(error))
}
