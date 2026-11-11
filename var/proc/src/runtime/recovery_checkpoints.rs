//! Bounded, session-owned recovery notes. A successful save means the transaction committed.
use super::{PostgresRuntime, RuntimeDbHandle, StateRuntime};
use chaos_ipc::ProcessId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;

pub const MAX_RECOVERY_CHECKPOINT_BYTES: usize = 16 * 1024;
pub const MAX_RECOVERY_CHECKPOINTS: i64 = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryCheckpointNote {
    pub summary: String,
    #[serde(default)]
    pub next_steps: Vec<String>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub running_tasks: Vec<String>,
}

impl RecoveryCheckpointNote {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.summary.trim().is_empty(),
            "checkpoint summary is required"
        );
        anyhow::ensure!(
            serde_json::to_vec(self)?.len() <= MAX_RECOVERY_CHECKPOINT_BYTES,
            "checkpoint exceeds the 16 KiB limit"
        );
        Ok(())
    }
}

/// Ownership, recovery, call identity, and observation come from the kernel, not tool JSON.
#[derive(Debug)]
pub struct RecoveryCheckpointWrite {
    pub process_id: ProcessId,
    /// None allocates the next durable, session-scoped recovery ID with this checkpoint.
    pub recovery_id: Option<i64>,
    pub call_id: String,
    pub note: RecoveryCheckpointNote,
    pub observation: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryCheckpoint {
    pub id: i64,
    pub process_id: ProcessId,
    pub recovery_id: i64,
    pub created_at: i64,
    pub note: RecoveryCheckpointNote,
    /// Historical host evidence, never a statement of current machine health.
    pub observation: Value,
}

/// A retry acknowledges the original note without allocating or rebinding a recovery ID.
#[derive(Debug)]
pub struct RecoveryCheckpointCommit {
    pub checkpoint: RecoveryCheckpoint,
    pub new_recovery_id: Option<i64>,
}

impl RuntimeDbHandle {
    chaos_dispatch::backend_dispatch! {
        pub async fn save_recovery_checkpoint(
            &self,
            write: &RecoveryCheckpointWrite,
        ) -> anyhow::Result<RecoveryCheckpointCommit>;
        pub async fn recovery_checkpoint(
            &self,
            process_id: ProcessId,
            id: Option<i64>,
        ) -> anyhow::Result<Option<RecoveryCheckpoint>>;
        pub async fn recovery_checkpoint_ids(
            &self,
            process_id: ProcessId,
        ) -> anyhow::Result<Vec<i64>>;
    }
}

fn decode<R: Row>(row: &R) -> anyhow::Result<RecoveryCheckpoint>
where
    for<'a> &'a str: sqlx::ColumnIndex<R>,
    for<'r> String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> i64: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
{
    Ok(RecoveryCheckpoint {
        id: row.try_get("id")?,
        process_id: ProcessId::from_string(&row.try_get::<String, _>("process_id")?)?,
        recovery_id: row.try_get("recovery_id")?,
        created_at: row.try_get("created_at")?,
        note: serde_json::from_str(&row.try_get::<String, _>("note_json")?)?,
        observation: serde_json::from_str(&row.try_get::<String, _>("observation_json")?)?,
    })
}

macro_rules! checkpoint_backend {
    ($runtime:ty) => {
        impl $runtime {
            async fn save_recovery_checkpoint(
                &self,
                write: &RecoveryCheckpointWrite,
            ) -> anyhow::Result<RecoveryCheckpointCommit> {
                write.note.validate()?;
                anyhow::ensure!(
                    !write.call_id.is_empty() && write.call_id.len() <= 256,
                    "invalid checkpoint call identity"
                );
                let note = serde_json::to_string(&write.note)?;
                let observation = serde_json::to_string(&write.observation)?;
                anyhow::ensure!(
                    observation.len() <= MAX_RECOVERY_CHECKPOINT_BYTES,
                    "checkpoint observation exceeds the 16 KiB limit"
                );
                let process = write.process_id.to_string();
                let mut tx = self.pool().begin().await?;
                // Serialize saves for one session on both backends, including retention.
                let owner =
                    sqlx::query("UPDATE processes SET updated_at = updated_at WHERE id = $1")
                        .bind(&process)
                        .execute(&mut *tx)
                        .await?;
                anyhow::ensure!(
                    owner.rows_affected() == 1,
                    "checkpoint session does not exist"
                );
                let existing = sqlx::query(
                    "SELECT * FROM recovery_checkpoints WHERE process_id = $1 AND call_id = $2",
                )
                .bind(&process)
                .bind(&write.call_id)
                .fetch_optional(&mut *tx)
                .await?;
                if let Some(row) = existing {
                    let checkpoint = decode(&row)?;
                    anyhow::ensure!(
                        checkpoint.note == write.note,
                        "checkpoint retry has different content; nothing was saved"
                    );
                    tx.commit().await?;
                    return Ok(RecoveryCheckpointCommit {
                        checkpoint,
                        new_recovery_id: None,
                    });
                }
                sqlx::query(
                    "INSERT INTO recovery_checkpoint_counters (process_id) VALUES ($1) \
                     ON CONFLICT (process_id) DO NOTHING",
                )
                .bind(&process)
                .execute(&mut *tx)
                .await?;
                let counters = sqlx::query(
                    "SELECT last_checkpoint_id, last_recovery_id \
                     FROM recovery_checkpoint_counters WHERE process_id = $1",
                )
                .bind(&process)
                .fetch_one(&mut *tx)
                .await?;
                let last_id: i64 = counters.try_get("last_checkpoint_id")?;
                let last_recovery_id: i64 = counters.try_get("last_recovery_id")?;
                let id = last_id
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("checkpoint sequence exhausted"))?;
                let recovery_id = match write.recovery_id {
                    Some(id) => {
                        anyhow::ensure!(
                            id > 0 && id <= last_recovery_id,
                            "recovery ID has not been issued in this session"
                        );
                        id
                    }
                    None => last_recovery_id
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("recovery sequence exhausted"))?,
                };
                // Never derive counters from prunable notes or restart-local memory.
                sqlx::query(
                    "UPDATE recovery_checkpoint_counters SET last_checkpoint_id = $2, \
                     last_recovery_id = $3 WHERE process_id = $1",
                )
                .bind(&process)
                .bind(id)
                .bind(last_recovery_id.max(recovery_id))
                .execute(&mut *tx)
                .await?;
                let row = sqlx::query(
                    "INSERT INTO recovery_checkpoints \
                     (process_id, id, recovery_id, call_id, created_at, \
                      note_json, observation_json) \
                     VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING *",
                )
                .bind(&process)
                .bind(id)
                .bind(recovery_id)
                .bind(&write.call_id)
                .bind(jiff::Timestamp::now().as_second())
                .bind(&note)
                .bind(&observation)
                .fetch_one(&mut *tx)
                .await?;
                let checkpoint = decode(&row)?;
                sqlx::query(
                    "DELETE FROM recovery_checkpoints WHERE process_id = $1 AND id NOT IN \
                     (SELECT id FROM recovery_checkpoints WHERE process_id = $1 \
                      ORDER BY id DESC LIMIT $2)",
                )
                .bind(&process)
                .bind(MAX_RECOVERY_CHECKPOINTS)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(RecoveryCheckpointCommit {
                    checkpoint,
                    new_recovery_id: write.recovery_id.is_none().then_some(recovery_id),
                })
            }

            async fn recovery_checkpoint(
                &self,
                process_id: ProcessId,
                id: Option<i64>,
            ) -> anyhow::Result<Option<RecoveryCheckpoint>> {
                let row = sqlx::query(
                    "SELECT * FROM recovery_checkpoints WHERE process_id = $1 \
                     AND (CAST($2 AS BIGINT) IS NULL OR id = $2) ORDER BY id DESC LIMIT 1",
                )
                .bind(process_id.to_string())
                .bind(id)
                .fetch_optional(self.pool())
                .await?;
                row.as_ref().map(decode).transpose()
            }

            async fn recovery_checkpoint_ids(
                &self,
                process_id: ProcessId,
            ) -> anyhow::Result<Vec<i64>> {
                let rows = sqlx::query(
                    "SELECT id FROM recovery_checkpoints WHERE process_id = $1 \
                     ORDER BY id DESC LIMIT $2",
                )
                .bind(process_id.to_string())
                .bind(MAX_RECOVERY_CHECKPOINTS)
                .fetch_all(self.pool())
                .await?;
                rows.iter().map(|row| Ok(row.try_get("id")?)).collect()
            }
        }
    };
}

checkpoint_backend!(StateRuntime);
checkpoint_backend!(PostgresRuntime);

#[cfg(test)]
mod tests;

#[cfg(test)]
mod postgres_tests;
