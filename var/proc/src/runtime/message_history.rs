use super::*;
use chaos_ipc::message_history::HistoryEntry;
use std::os::unix::fs::MetadataExt;

const HISTORY_SOFT_CAP_RATIO: f64 = 0.8;

impl StateRuntime {
    pub async fn append_message_history_entry(
        &self,
        entry: &HistoryEntry,
        max_bytes: Option<usize>,
    ) -> anyhow::Result<()> {
        let estimated_bytes = estimated_history_entry_bytes(entry)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
INSERT INTO message_history (conversation_id, ts, text, estimated_bytes)
VALUES (?, ?, ?, ?)
            "#,
        )
        .bind(&entry.conversation_id)
        .bind(i64::try_from(entry.ts).unwrap_or(i64::MAX))
        .bind(&entry.text)
        .bind(estimated_bytes)
        .execute(&mut *tx)
        .await?;
        prune_message_history_after_insert(estimated_bytes, max_bytes, &mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn message_history_metadata(&self) -> anyhow::Result<(u64, usize)> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM message_history")
            .fetch_one(self.pool.as_ref())
            .await?;
        Ok((
            message_history_log_id(self.chaos_home()).unwrap_or(0),
            usize::try_from(count).unwrap_or(0),
        ))
    }

    pub async fn get_message_history_entry(
        &self,
        log_id: u64,
        offset: usize,
    ) -> anyhow::Result<Option<HistoryEntry>> {
        let current_log_id = message_history_log_id(self.chaos_home()).unwrap_or(0);
        if log_id != 0 && current_log_id != 0 && current_log_id != log_id {
            return Ok(None);
        }

        let row = sqlx::query(
            r#"
SELECT conversation_id, ts, text
FROM message_history
ORDER BY id ASC
LIMIT 1 OFFSET ?
            "#,
        )
        .bind(i64::try_from(offset).unwrap_or(i64::MAX))
        .fetch_optional(self.pool.as_ref())
        .await?;

        row.map(|row| -> anyhow::Result<HistoryEntry> {
            Ok(HistoryEntry {
                conversation_id: row.try_get("conversation_id")?,
                ts: u64::try_from(row.try_get::<i64, _>("ts")?).unwrap_or(0),
                text: row.try_get("text")?,
            })
        })
        .transpose()
    }
}

fn estimated_history_entry_bytes(entry: &HistoryEntry) -> anyhow::Result<i64> {
    let mut serialized = serde_json::to_string(entry)?;
    serialized.push('\n');
    Ok(i64::try_from(serialized.len()).unwrap_or(i64::MAX))
}

fn trim_target_bytes(max_bytes: i64, newest_entry_len: i64) -> i64 {
    let soft_cap_bytes = ((max_bytes as f64) * HISTORY_SOFT_CAP_RATIO)
        .floor()
        .clamp(1.0, max_bytes as f64) as i64;
    soft_cap_bytes.max(newest_entry_len)
}

async fn prune_message_history_after_insert(
    newest_entry_len: i64,
    max_bytes: Option<usize>,
    tx: &mut SqliteConnection,
) -> anyhow::Result<()> {
    let Some(max_bytes) = max_bytes else {
        return Ok(());
    };
    if max_bytes == 0 {
        return Ok(());
    }

    let max_bytes = i64::try_from(max_bytes).unwrap_or(i64::MAX);
    let total_bytes: i64 =
        sqlx::query_scalar("SELECT COALESCE(SUM(estimated_bytes), 0) FROM message_history")
            .fetch_one(&mut *tx)
            .await?;

    if total_bytes <= max_bytes {
        return Ok(());
    }

    let trim_target = trim_target_bytes(max_bytes, newest_entry_len);
    sqlx::query(
        r#"
DELETE FROM message_history
WHERE id IN (
    SELECT id
    FROM (
        SELECT
            id,
            SUM(estimated_bytes) OVER (ORDER BY id DESC) AS cumulative_bytes
        FROM message_history
    )
    WHERE cumulative_bytes > ?
)
        "#,
    )
    .bind(trim_target)
    .execute(&mut *tx)
    .await?;

    Ok(())
}

fn message_history_log_id(chaos_home: &Path) -> Option<u64> {
    std::fs::metadata(runtime_db_path(chaos_home))
        .ok()
        .map(|metadata| metadata.ino())
}
