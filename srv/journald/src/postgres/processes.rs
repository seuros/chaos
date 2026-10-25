use std::time::Duration;

use chaos_ipc::ProcessId;
use uuid::Uuid;

use super::PostgresJournalStore;
use super::codec::{process_row_to_record, serialize_source};
use super::transactions::insert_journal_entry;
use crate::error::JournalError;
use crate::model::{
    CreateProcessInput, InitializeProcessInput, InitializeProcessResult, Lease, ProcessRecord,
};
use crate::store::{
    new_process_record, process_insert_error, source_label, timestamp_after, validate_sequences,
};

impl PostgresJournalStore {
    pub(super) async fn create_process_inner(
        &self,
        input: CreateProcessInput,
    ) -> Result<ProcessRecord, JournalError> {
        let created_at = input.created_at;
        let input = new_process_record(input, created_at);
        let source = source_label(&input.source);
        let source_json = serialize_source(&input.source)?;
        let created_at = input.created_at.as_second();
        let parent_process_id = input
            .parent
            .as_ref()
            .map(|parent| parent.parent_process_id.to_string());
        let fork_at_seq = input.parent.as_ref().map(|parent| parent.fork_at_seq);

        let result = sqlx::query(
            "INSERT INTO processes (
                id,
                parent_process_id,
                fork_at_seq,
                source,
                source_json,
                cwd,
                created_at,
                updated_at,
                archived_at,
                title,
                model_provider,
                cli_version,
                agent_nickname,
                agent_role
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, $9, $10, $11, $12, $13)",
        )
        .bind(input.process_id.to_string())
        .bind(parent_process_id)
        .bind(fork_at_seq)
        .bind(source)
        .bind(source_json)
        .bind(input.cwd.to_string_lossy().to_string())
        .bind(created_at)
        .bind(created_at)
        .bind(&input.title)
        .bind(&input.model_provider)
        .bind(input.cli_version.as_deref().unwrap_or_default())
        .bind(&input.agent_nickname)
        .bind(&input.agent_role)
        .execute(&self.pool)
        .await;

        result.map_err(|err| process_insert_error(input.process_id, err))?;
        Ok(input)
    }

    pub(super) async fn initialize_process_inner(
        &self,
        input: InitializeProcessInput,
    ) -> Result<InitializeProcessResult, JournalError> {
        if input.items.is_empty() {
            return Err(JournalError::InvalidRequest(format!(
                "initialize_process for {} requires at least one journal entry; \
                 use create_process + acquire_lease for empty-journal cases",
                input.create.process_id
            )));
        }
        validate_sequences(input.create.process_id, 0, &input.items)?;

        let create = &input.create;
        let source = source_label(&create.source);
        let source_json = serialize_source(&create.source)?;
        let parent_process_id = create
            .parent
            .as_ref()
            .map(|parent| parent.parent_process_id.to_string());
        let fork_at_seq = create.parent.as_ref().map(|parent| parent.fork_at_seq);

        let now = jiff::Timestamp::now();
        let lease_expires_at = timestamp_after(now, Duration::from_millis(input.ttl_ms))?;
        let lease_token = Uuid::now_v7().to_string();
        let updated_at = input
            .items
            .last()
            .map(|entry| entry.recorded_at)
            .unwrap_or(now);
        let next_seq = input.items.last().map(|entry| entry.seq + 1).unwrap_or(0);
        let create = new_process_record(input.create, updated_at);

        let mut tx = self.pool.begin().await?;
        let insert_process = sqlx::query(
            "INSERT INTO processes (
                id,
                parent_process_id,
                fork_at_seq,
                source,
                source_json,
                cwd,
                created_at,
                updated_at,
                archived_at,
                title,
                model_provider,
                cli_version,
                agent_nickname,
                agent_role
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, $9, $10, $11, $12, $13)",
        )
        .bind(create.process_id.to_string())
        .bind(parent_process_id)
        .bind(fork_at_seq)
        .bind(&source)
        .bind(&source_json)
        .bind(create.cwd.to_string_lossy().to_string())
        .bind(create.created_at.as_second())
        .bind(updated_at.as_second())
        .bind(&create.title)
        .bind(&create.model_provider)
        .bind(create.cli_version.as_deref().unwrap_or_default())
        .bind(&create.agent_nickname)
        .bind(&create.agent_role)
        .execute(&mut *tx)
        .await;
        insert_process.map_err(|err| process_insert_error(create.process_id, err))?;

        for entry in &input.items {
            insert_journal_entry(&mut tx, &create.process_id, entry).await?;
        }

        sqlx::query(
            "INSERT INTO process_leases (process_id, owner_id, lease_token, expires_at, updated_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(create.process_id.to_string())
        .bind(&input.owner_id)
        .bind(&lease_token)
        .bind(lease_expires_at.as_second())
        .bind(now.as_second())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(InitializeProcessResult {
            lease: Lease {
                process_id: create.process_id,
                owner_id: input.owner_id,
                lease_token,
                expires_at: lease_expires_at,
            },
            process: create,
            next_seq,
            updated_at,
        })
    }

    pub(super) async fn get_process_inner(
        &self,
        process_id: &ProcessId,
    ) -> Result<Option<ProcessRecord>, JournalError> {
        let row = sqlx::query(process_select_sql!("WHERE p.id = $1"))
            .bind(process_id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(process_row_to_record).transpose()
    }

    pub(super) async fn list_processes_inner(
        &self,
        archived: Option<bool>,
    ) -> Result<Vec<ProcessRecord>, JournalError> {
        let rows = sqlx::query(process_select_sql!(
            "WHERE (
                $1::BOOLEAN IS NULL
                OR ($1 = TRUE AND p.archived_at IS NOT NULL)
                OR ($1 = FALSE AND p.archived_at IS NULL)
             )",
        ))
        .bind(archived)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(process_row_to_record).collect()
    }
}
