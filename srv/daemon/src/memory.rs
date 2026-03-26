use crate::schema::Memory;
use crate::{Daemon, DaemonError};

impl Daemon {
    /// Remember something.
    pub async fn remember(
        &self,
        scope: &str,
        category: &str,
        content: &str,
    ) -> Result<Memory, DaemonError> {
        let row = sqlx::query_as::<_, Memory>(
            "INSERT INTO memories (scope, category, content)
             VALUES (?1, ?2, ?3)
             RETURNING *",
        )
        .bind(scope)
        .bind(category)
        .bind(content)
        .fetch_one(self.pool())
        .await?;

        Ok(row)
    }

    /// Recall memories, ordered by relevance (access_count * confidence, most recent first).
    pub async fn recall(
        &self,
        scope: &str,
        category: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Memory>, DaemonError> {
        let rows = sqlx::query_as::<_, Memory>(
            "SELECT * FROM memories
             WHERE (scope = ?1 OR scope = 'global')
               AND (?2 IS NULL OR category = ?2)
             ORDER BY (access_count * confidence) DESC, accessed_at DESC
             LIMIT ?3",
        )
        .bind(scope)
        .bind(category)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        // Bump access stats for returned memories
        for m in &rows {
            sqlx::query(
                "UPDATE memories SET accessed_at = unixepoch(), access_count = access_count + 1 WHERE id = ?1"
            )
            .bind(m.id)
            .execute(self.pool())
            .await?;
        }

        Ok(rows)
    }

    /// Reinforce a memory — boost confidence.
    pub async fn reinforce(&self, id: i64, boost: f64) -> Result<(), DaemonError> {
        let result =
            sqlx::query("UPDATE memories SET confidence = MIN(confidence + ?2, 1.0) WHERE id = ?1")
                .bind(id)
                .bind(boost)
                .execute(self.pool())
                .await?;

        if result.rows_affected() == 0 {
            return Err(DaemonError::MemoryNotFound(id));
        }
        Ok(())
    }

    /// Decay unused memories. Call periodically.
    /// Memories not accessed in `stale_days` lose confidence.
    pub async fn decay(&self, stale_days: i64, decay_rate: f64) -> Result<u64, DaemonError> {
        let cutoff = jiff::Timestamp::now().as_second() - (stale_days * 86400);
        let result = sqlx::query(
            "UPDATE memories SET confidence = MAX(confidence - ?2, 0.0)
             WHERE accessed_at < ?1 AND confidence > 0.0",
        )
        .bind(cutoff)
        .bind(decay_rate)
        .execute(self.pool())
        .await?;

        Ok(result.rows_affected())
    }

    /// Forget a specific memory.
    pub async fn forget(&self, id: i64) -> Result<(), DaemonError> {
        let result = sqlx::query("DELETE FROM memories WHERE id = ?1")
            .bind(id)
            .execute(self.pool())
            .await?;

        if result.rows_affected() == 0 {
            return Err(DaemonError::MemoryNotFound(id));
        }
        Ok(())
    }

    /// Purge dead memories (confidence = 0).
    pub async fn purge_dead(&self) -> Result<u64, DaemonError> {
        let result = sqlx::query("DELETE FROM memories WHERE confidence <= 0.0")
            .execute(self.pool())
            .await?;
        Ok(result.rows_affected())
    }
}
