use super::super::super::{
    BTreeSet, LOG_PARTITION_ROW_LIMIT, LOG_PARTITION_SIZE_LIMIT_BYTES, LogEntry, QueryBuilder, Row,
    Sqlite, SqliteConnection, StateRuntime,
};
use super::super::prune::logs_prune_backend;

logs_prune_backend!(StateRuntime, Sqlite, SqliteConnection);

impl StateRuntime {
    pub async fn insert_log(&self, entry: &LogEntry) -> anyhow::Result<()> {
        self.insert_logs(std::slice::from_ref(entry)).await
    }

    /// Insert a batch of log entries into the logs table.
    pub async fn insert_logs(&self, entries: &[LogEntry]) -> anyhow::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        let mut tx = self.pool.begin().await?;
        let mut builder = QueryBuilder::<Sqlite>::new(
            "INSERT INTO logs (ts, ts_nanos, level, target, message, process_id, process_uuid, module_path, file, line, estimated_bytes) ",
        );
        builder.push_values(entries, |mut row, entry| {
            let estimated_bytes = entry.message.as_ref().map_or(0, String::len) as i64
                + entry.level.len() as i64
                + entry.target.len() as i64
                + entry.module_path.as_ref().map_or(0, String::len) as i64
                + entry.file.as_ref().map_or(0, String::len) as i64;
            row.push_bind(entry.ts)
                .push_bind(entry.ts_nanos)
                .push_bind(&entry.level)
                .push_bind(&entry.target)
                .push_bind(&entry.message)
                .push_bind(&entry.process_id)
                .push_bind(&entry.process_uuid)
                .push_bind(&entry.module_path)
                .push_bind(&entry.file)
                .push_bind(entry.line)
                .push_bind(estimated_bytes);
        });
        builder.build().execute(&mut *tx).await?;
        self.prune_logs_after_insert(entries, &mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_logs_before(&self, cutoff_ts: i64) -> anyhow::Result<u64> {
        let result = sqlx::query("DELETE FROM logs WHERE ts < ?")
            .bind(cutoff_ts)
            .execute(self.pool.as_ref())
            .await?;
        Ok(result.rows_affected())
    }
}
