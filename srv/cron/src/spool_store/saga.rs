//! Submit-saga persistence next to the manifest in `spool_jobs.saga_json`.

use super::PostgresSpoolStore;
use super::SpoolRow;
use super::SpoolStore;

impl SpoolStore {
    pub async fn save_saga(
        &self,
        manifest_id: &str,
        saga_json: Option<&str>,
        batch_id: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE spool_jobs SET saga_json = ?, batch_id = COALESCE(?, batch_id) \
             WHERE manifest_id = ?",
        )
        .bind(saga_json)
        .bind(batch_id)
        .bind(manifest_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_pending_sagas(&self) -> anyhow::Result<Vec<SpoolRow>> {
        let rows = sqlx::query_as::<_, SpoolRow>(
            "SELECT manifest_id, backend, batch_id, status, request_count, payload_json, \
                    result_json, raw_result, error, submitted_at, completed_at, \
                    created_at, updated_at, saga_json \
             FROM spool_jobs WHERE saga_json IS NOT NULL ORDER BY created_at ASC, manifest_id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

impl PostgresSpoolStore {
    pub async fn save_saga(
        &self,
        manifest_id: &str,
        saga_json: Option<&str>,
        batch_id: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE spool_jobs SET saga_json = $1, batch_id = COALESCE($2, batch_id) \
             WHERE manifest_id = $3",
        )
        .bind(saga_json)
        .bind(batch_id)
        .bind(manifest_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_pending_sagas(&self) -> anyhow::Result<Vec<SpoolRow>> {
        let rows = sqlx::query_as::<_, SpoolRow>(
            "SELECT manifest_id, backend, batch_id, status, request_count, payload_json, \
                    result_json, raw_result, error, submitted_at, completed_at, \
                    created_at, updated_at, saga_json \
             FROM spool_jobs WHERE saga_json IS NOT NULL ORDER BY created_at ASC, manifest_id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}
