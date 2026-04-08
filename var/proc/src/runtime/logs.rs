use super::*;
use crate::LogTailBatch;
use crate::LogTailCursor;

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

    /// Enforce per-partition log size caps after a successful batch insert.
    ///
    /// We maintain two independent budgets:
    /// - Thread logs: rows with `process_id IS NOT NULL`, capped per `process_id`.
    /// - Threadless process logs: rows with `process_id IS NULL` ("processless"),
    ///   capped per `process_uuid` (including `process_uuid IS NULL` as its own
    ///   processless partition).
    ///
    /// "Threadless" means the log row is not associated with any conversation
    /// thread, so retention is keyed by process identity instead.
    ///
    /// This runs inside the same transaction as the insert so callers never
    /// observe "inserted but not yet pruned" rows.
    async fn prune_logs_after_insert(
        &self,
        entries: &[LogEntry],
        tx: &mut SqliteConnection,
    ) -> anyhow::Result<()> {
        let process_ids: BTreeSet<&str> = entries
            .iter()
            .filter_map(|entry| entry.process_id.as_deref())
            .collect();
        if !process_ids.is_empty() {
            // Cheap precheck: only run the heavier window-function prune for
            // processes that are currently above the cap.
            let mut over_limit_threads_query =
                QueryBuilder::<Sqlite>::new("SELECT process_id FROM logs WHERE process_id IN (");
            {
                let mut separated = over_limit_threads_query.separated(", ");
                for process_id in &process_ids {
                    separated.push_bind(*process_id);
                }
            }
            over_limit_threads_query.push(") GROUP BY process_id HAVING SUM(");
            over_limit_threads_query.push("estimated_bytes");
            over_limit_threads_query.push(") > ");
            over_limit_threads_query.push_bind(LOG_PARTITION_SIZE_LIMIT_BYTES);
            over_limit_threads_query.push(" OR COUNT(*) > ");
            over_limit_threads_query.push_bind(LOG_PARTITION_ROW_LIMIT);
            let over_limit_process_ids: Vec<String> = over_limit_threads_query
                .build()
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|row| row.try_get("process_id"))
                .collect::<Result<_, _>>()?;
            if !over_limit_process_ids.is_empty() {
                // Enforce a strict per-thread cap by deleting every row whose
                // newest-first cumulative bytes exceed the partition budget.
                let mut prune_threads = QueryBuilder::<Sqlite>::new(
                    r#"
DELETE FROM logs
WHERE id IN (
    SELECT id
    FROM (
        SELECT
            id,
            SUM(
"#,
                );
                prune_threads.push("estimated_bytes");
                prune_threads.push(
                    r#"
            ) OVER (
                PARTITION BY process_id
                ORDER BY ts DESC, ts_nanos DESC, id DESC
            ) AS cumulative_bytes,
            ROW_NUMBER() OVER (
                PARTITION BY process_id
                ORDER BY ts DESC, ts_nanos DESC, id DESC
            ) AS row_number
        FROM logs
        WHERE process_id IN (
"#,
                );
                {
                    let mut separated = prune_threads.separated(", ");
                    for process_id in &over_limit_process_ids {
                        separated.push_bind(process_id);
                    }
                }
                prune_threads.push(
                    r#"
        )
    )
    WHERE cumulative_bytes >
"#,
                );
                prune_threads.push_bind(LOG_PARTITION_SIZE_LIMIT_BYTES);
                prune_threads.push(" OR row_number > ");
                prune_threads.push_bind(LOG_PARTITION_ROW_LIMIT);
                prune_threads.push("\n)");
                prune_threads.build().execute(&mut *tx).await?;
            }
        }

        let processless_process_uuids: BTreeSet<&str> = entries
            .iter()
            .filter(|entry| entry.process_id.is_none())
            .filter_map(|entry| entry.process_uuid.as_deref())
            .collect();
        let has_processless_null_process_uuid = entries
            .iter()
            .any(|entry| entry.process_id.is_none() && entry.process_uuid.is_none());
        if !processless_process_uuids.is_empty() {
            // Threadless logs are budgeted separately per process UUID.
            let mut over_limit_processes_query = QueryBuilder::<Sqlite>::new(
                "SELECT process_uuid FROM logs WHERE process_id IS NULL AND process_uuid IN (",
            );
            {
                let mut separated = over_limit_processes_query.separated(", ");
                for process_uuid in &processless_process_uuids {
                    separated.push_bind(*process_uuid);
                }
            }
            over_limit_processes_query.push(") GROUP BY process_uuid HAVING SUM(");
            over_limit_processes_query.push("estimated_bytes");
            over_limit_processes_query.push(") > ");
            over_limit_processes_query.push_bind(LOG_PARTITION_SIZE_LIMIT_BYTES);
            over_limit_processes_query.push(" OR COUNT(*) > ");
            over_limit_processes_query.push_bind(LOG_PARTITION_ROW_LIMIT);
            let over_limit_process_uuids: Vec<String> = over_limit_processes_query
                .build()
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|row| row.try_get("process_uuid"))
                .collect::<Result<_, _>>()?;
            if !over_limit_process_uuids.is_empty() {
                // Same strict cap policy as thread pruning, but only for
                // processless rows in the affected process UUIDs.
                let mut prune_processless_process_logs = QueryBuilder::<Sqlite>::new(
                    r#"
DELETE FROM logs
WHERE id IN (
    SELECT id
    FROM (
        SELECT
            id,
            SUM(
"#,
                );
                prune_processless_process_logs.push("estimated_bytes");
                prune_processless_process_logs.push(
                    r#"
            ) OVER (
                PARTITION BY process_uuid
                ORDER BY ts DESC, ts_nanos DESC, id DESC
            ) AS cumulative_bytes,
            ROW_NUMBER() OVER (
                PARTITION BY process_uuid
                ORDER BY ts DESC, ts_nanos DESC, id DESC
            ) AS row_number
        FROM logs
        WHERE process_id IS NULL
          AND process_uuid IN (
"#,
                );
                {
                    let mut separated = prune_processless_process_logs.separated(", ");
                    for process_uuid in &over_limit_process_uuids {
                        separated.push_bind(process_uuid);
                    }
                }
                prune_processless_process_logs.push(
                    r#"
          )
    )
    WHERE cumulative_bytes >
"#,
                );
                prune_processless_process_logs.push_bind(LOG_PARTITION_SIZE_LIMIT_BYTES);
                prune_processless_process_logs.push(" OR row_number > ");
                prune_processless_process_logs.push_bind(LOG_PARTITION_ROW_LIMIT);
                prune_processless_process_logs.push("\n)");
                prune_processless_process_logs
                    .build()
                    .execute(&mut *tx)
                    .await?;
            }
        }
        if has_processless_null_process_uuid {
            // Rows without a process UUID still need a cap; treat NULL as its
            // own processless partition.
            let mut null_process_usage_query = QueryBuilder::<Sqlite>::new("SELECT SUM(");
            null_process_usage_query.push("estimated_bytes");
            null_process_usage_query.push(
                ") AS total_bytes, COUNT(*) AS row_count FROM logs WHERE process_id IS NULL AND process_uuid IS NULL",
            );
            let null_process_usage = null_process_usage_query.build().fetch_one(&mut *tx).await?;
            let total_null_process_bytes: Option<i64> =
                null_process_usage.try_get("total_bytes")?;
            let null_process_row_count: i64 = null_process_usage.try_get("row_count")?;

            if total_null_process_bytes.unwrap_or(0) > LOG_PARTITION_SIZE_LIMIT_BYTES
                || null_process_row_count > LOG_PARTITION_ROW_LIMIT
            {
                let mut prune_processless_null_process_logs = QueryBuilder::<Sqlite>::new(
                    r#"
DELETE FROM logs
WHERE id IN (
    SELECT id
    FROM (
        SELECT
            id,
            SUM(
"#,
                );
                prune_processless_null_process_logs.push("estimated_bytes");
                prune_processless_null_process_logs.push(
                    r#"
            ) OVER (
                PARTITION BY process_uuid
                ORDER BY ts DESC, ts_nanos DESC, id DESC
            ) AS cumulative_bytes,
            ROW_NUMBER() OVER (
                PARTITION BY process_uuid
                ORDER BY ts DESC, ts_nanos DESC, id DESC
            ) AS row_number
        FROM logs
        WHERE process_id IS NULL
          AND process_uuid IS NULL
    )
    WHERE cumulative_bytes >
"#,
                );
                prune_processless_null_process_logs.push_bind(LOG_PARTITION_SIZE_LIMIT_BYTES);
                prune_processless_null_process_logs.push(" OR row_number > ");
                prune_processless_null_process_logs.push_bind(LOG_PARTITION_ROW_LIMIT);
                prune_processless_null_process_logs.push("\n)");
                prune_processless_null_process_logs
                    .build()
                    .execute(&mut *tx)
                    .await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn delete_logs_before(&self, cutoff_ts: i64) -> anyhow::Result<u64> {
        let result = sqlx::query("DELETE FROM logs WHERE ts < ?")
            .bind(cutoff_ts)
            .execute(self.pool.as_ref())
            .await?;
        Ok(result.rows_affected())
    }

    /// Query logs with optional filters.
    pub async fn query_logs(&self, query: &LogQuery) -> anyhow::Result<Vec<LogRow>> {
        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT id, ts, ts_nanos, level, target, message, process_id, process_uuid, file, line FROM logs WHERE 1 = 1",
        );
        push_log_filters(&mut builder, query);
        if query.descending {
            builder.push(" ORDER BY id DESC");
        } else {
            builder.push(" ORDER BY id ASC");
        }
        if let Some(limit) = query.limit {
            builder.push(" LIMIT ").push_bind(limit as i64);
        }

        let rows = builder
            .build_query_as::<LogRow>()
            .fetch_all(self.pool.as_ref())
            .await?;
        Ok(rows)
    }

    /// Return the most recent matching logs in ascending order.
    ///
    /// Internally this queries newest-first for efficiency, then reverses the
    /// result so consumers can render in natural chronological order.
    pub async fn recent_logs(&self, query: &LogQuery, limit: usize) -> anyhow::Result<Vec<LogRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let mut recent_query = query.clone();
        recent_query.limit = Some(limit);
        recent_query.after_id = None;
        recent_query.descending = true;

        let mut rows = self.query_logs(&recent_query).await?;
        rows.reverse();
        Ok(rows)
    }

    /// Return a backfill batch for a tailing consumer, plus the cursor to use
    /// for subsequent incremental polls.
    pub async fn tail_backfill(
        &self,
        query: &LogQuery,
        limit: usize,
    ) -> anyhow::Result<LogTailBatch> {
        let rows = self.recent_logs(query, limit).await?;
        let last_id = rows
            .last()
            .map(|row| row.id)
            .unwrap_or(self.max_log_id(query).await?);
        Ok(LogTailBatch {
            rows,
            cursor: LogTailCursor { last_id },
        })
    }

    /// Return matching logs that were inserted after the provided row id.
    ///
    /// Results are always returned in ascending order so callers can append
    /// them directly to a live view.
    pub async fn query_logs_after(
        &self,
        query: &LogQuery,
        after_id: i64,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<LogRow>> {
        let mut next_query = query.clone();
        next_query.after_id = Some(after_id);
        next_query.limit = limit;
        next_query.descending = false;
        self.query_logs(&next_query).await
    }

    /// Poll for rows after the provided cursor and return the advanced cursor.
    pub async fn tail_poll(
        &self,
        query: &LogQuery,
        cursor: &LogTailCursor,
        limit: Option<usize>,
    ) -> anyhow::Result<LogTailBatch> {
        let rows = self.query_logs_after(query, cursor.last_id, limit).await?;
        let last_id = rows.last().map(|row| row.id).unwrap_or(cursor.last_id);
        Ok(LogTailBatch {
            rows,
            cursor: LogTailCursor { last_id },
        })
    }

    /// Query per-thread feedback logs, capped to the per-thread SQLite retention budget.
    pub async fn query_feedback_logs(&self, process_id: &str) -> anyhow::Result<Vec<u8>> {
        let max_bytes = LOG_PARTITION_SIZE_LIMIT_BYTES;
        // TODO(ccunningham): Store rendered span/event fields in SQLite so this
        // export can match feedback formatting beyond timestamp + level + message.
        let lines = sqlx::query_scalar::<_, String>(
            r#"
WITH latest_process AS (
    SELECT process_uuid
    FROM logs
    WHERE process_id = ? AND process_uuid IS NOT NULL
    ORDER BY ts DESC, ts_nanos DESC, id DESC
    LIMIT 1
),
feedback_logs AS (
    SELECT
        printf(
            '%s.%06dZ %5s %s',
            strftime('%Y-%m-%dT%H:%M:%S', ts, 'unixepoch'),
            ts_nanos / 1000,
            level,
            message
        ) || CASE
            WHEN substr(message, -1, 1) = char(10) THEN ''
            ELSE char(10)
        END AS line,
        length(CAST(
            printf(
                '%s.%06dZ %5s %s',
                strftime('%Y-%m-%dT%H:%M:%S', ts, 'unixepoch'),
                ts_nanos / 1000,
                level,
                message
            ) || CASE
                WHEN substr(message, -1, 1) = char(10) THEN ''
                ELSE char(10)
            END AS BLOB
        )) AS line_bytes,
        ts,
        ts_nanos,
        id
    FROM logs
    WHERE message IS NOT NULL AND (
        process_id = ?
        OR (
            process_id IS NULL
            AND process_uuid IN (SELECT process_uuid FROM latest_process)
        )
    )
)
SELECT line
FROM (
    SELECT
        line,
        ts,
        ts_nanos,
        id,
        SUM(line_bytes) OVER (
            ORDER BY ts DESC, ts_nanos DESC, id DESC
        ) AS cumulative_bytes
    FROM feedback_logs
)
WHERE cumulative_bytes <= ?
ORDER BY ts ASC, ts_nanos ASC, id ASC
"#,
        )
        .bind(process_id)
        .bind(process_id)
        .bind(max_bytes)
        .fetch_all(self.pool.as_ref())
        .await?;

        Ok(lines.concat().into_bytes())
    }

    /// Return the max log id matching optional filters.
    pub async fn max_log_id(&self, query: &LogQuery) -> anyhow::Result<i64> {
        let mut builder =
            QueryBuilder::<Sqlite>::new("SELECT MAX(id) AS max_id FROM logs WHERE 1 = 1");
        push_log_filters(&mut builder, query);
        let row = builder.build().fetch_one(self.pool.as_ref()).await?;
        let max_id: Option<i64> = row.try_get("max_id")?;
        Ok(max_id.unwrap_or(0))
    }
}

fn push_log_filters<'a>(builder: &mut QueryBuilder<'a, Sqlite>, query: &'a LogQuery) {
    if let Some(level_upper) = query.level_upper.as_ref() {
        builder
            .push(" AND UPPER(level) = ")
            .push_bind(level_upper.as_str());
    }
    if let Some(from_ts) = query.from_ts {
        builder.push(" AND ts >= ").push_bind(from_ts);
    }
    if let Some(to_ts) = query.to_ts {
        builder.push(" AND ts <= ").push_bind(to_ts);
    }
    push_like_filters(builder, "module_path", &query.module_like);
    push_like_filters(builder, "file", &query.file_like);
    if let Some(process_id) = query.related_to_process_id.as_ref() {
        builder.push(" AND (");
        builder.push("process_id = ").push_bind(process_id.as_str());
        if query.include_related_processless {
            builder.push(" OR (process_id IS NULL AND process_uuid IN (");
            builder.push("SELECT process_uuid FROM logs WHERE process_id = ");
            builder.push_bind(process_id.as_str());
            builder.push(
                " AND process_uuid IS NOT NULL ORDER BY ts DESC, ts_nanos DESC, id DESC LIMIT 1",
            );
            builder.push("))");
        }
        builder.push(")");
    } else {
        let has_process_filter = !query.process_ids.is_empty() || query.include_processless;
        if has_process_filter {
            builder.push(" AND (");
            let mut needs_or = false;
            for process_id in &query.process_ids {
                if needs_or {
                    builder.push(" OR ");
                }
                builder.push("process_id = ").push_bind(process_id.as_str());
                needs_or = true;
            }
            if query.include_processless {
                if needs_or {
                    builder.push(" OR ");
                }
                builder.push("process_id IS NULL");
            }
            builder.push(")");
        }
    }
    if let Some(after_id) = query.after_id {
        builder.push(" AND id > ").push_bind(after_id);
    }
    if let Some(search) = query.search.as_ref() {
        builder.push(" AND INSTR(message, ");
        builder.push_bind(search.as_str());
        builder.push(") > 0");
    }
}

fn push_like_filters<'a>(
    builder: &mut QueryBuilder<'a, Sqlite>,
    column: &str,
    filters: &'a [String],
) {
    if filters.is_empty() {
        return;
    }
    builder.push(" AND (");
    for (idx, filter) in filters.iter().enumerate() {
        if idx > 0 {
            builder.push(" OR ");
        }
        builder
            .push(column)
            .push(" LIKE '%' || ")
            .push_bind(filter.as_str())
            .push(" || '%'");
    }
    builder.push(")");
}

#[cfg(test)]
mod tests {
    use super::StateRuntime;
    use super::test_support::unique_temp_dir;
    use crate::LogEntry;
    use crate::LogQuery;
    use crate::runtime_db_path;
    use pretty_assertions::assert_eq;
    use sqlx::SqlitePool;
    use sqlx::sqlite::SqliteConnectOptions;
    use std::path::Path;

    async fn open_db_pool(path: &Path) -> SqlitePool {
        SqlitePool::connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(false),
        )
        .await
        .expect("open sqlite pool")
    }

    async fn log_row_count(path: &Path) -> i64 {
        let pool = open_db_pool(path).await;
        let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM logs")
            .fetch_one(&pool)
            .await
            .expect("count log rows");
        pool.close().await;
        count
    }

    #[tokio::test]
    async fn insert_logs_persist_into_runtime_database() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        runtime
            .insert_logs(&[LogEntry {
                ts: 1,
                ts_nanos: 0,
                level: "INFO".to_string(),
                target: "cli".to_string(),
                message: Some("dedicated-log-db".to_string()),
                process_id: Some("thread-1".to_string()),
                process_uuid: Some("proc-1".to_string()),
                module_path: Some("mod".to_string()),
                file: Some("main.rs".to_string()),
                line: Some(7),
            }])
            .await
            .expect("insert test logs");

        let logs_count = log_row_count(runtime_db_path(chaos_home.as_path()).as_path()).await;

        assert_eq!(logs_count, 1);

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn query_logs_with_search_matches_substring() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        runtime
            .insert_logs(&[
                LogEntry {
                    ts: 1_700_000_001,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("alpha".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: None,
                    file: Some("main.rs".to_string()),
                    line: Some(42),
                    module_path: None,
                },
                LogEntry {
                    ts: 1_700_000_002,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("alphabet".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: None,
                    file: Some("main.rs".to_string()),
                    line: Some(43),
                    module_path: None,
                },
            ])
            .await
            .expect("insert test logs");

        let rows = runtime
            .query_logs(&LogQuery {
                search: Some("alphab".to_string()),
                ..Default::default()
            })
            .await
            .expect("query matching logs");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message.as_deref(), Some("alphabet"));

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn recent_logs_returns_latest_rows_in_ascending_order() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        runtime
            .insert_logs(&[
                LogEntry {
                    ts: 10,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("first".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: None,
                    file: Some("main.rs".to_string()),
                    line: Some(1),
                    module_path: None,
                },
                LogEntry {
                    ts: 11,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("second".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: None,
                    file: Some("main.rs".to_string()),
                    line: Some(2),
                    module_path: None,
                },
                LogEntry {
                    ts: 12,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("third".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: None,
                    file: Some("main.rs".to_string()),
                    line: Some(3),
                    module_path: None,
                },
            ])
            .await
            .expect("insert test logs");

        let rows = runtime
            .recent_logs(&LogQuery::default(), 2)
            .await
            .expect("query recent logs");

        let messages = rows
            .iter()
            .map(|row| row.message.as_deref().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(messages, vec!["second", "third"]);

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn query_logs_after_returns_only_newer_rows_in_ascending_order() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        runtime
            .insert_logs(&[
                LogEntry {
                    ts: 20,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("one".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: None,
                    file: Some("main.rs".to_string()),
                    line: Some(1),
                    module_path: None,
                },
                LogEntry {
                    ts: 21,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("two".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: None,
                    file: Some("main.rs".to_string()),
                    line: Some(2),
                    module_path: None,
                },
                LogEntry {
                    ts: 22,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("three".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: None,
                    file: Some("main.rs".to_string()),
                    line: Some(3),
                    module_path: None,
                },
            ])
            .await
            .expect("insert test logs");

        let backfill = runtime
            .recent_logs(&LogQuery::default(), 2)
            .await
            .expect("query recent logs");
        let last_id = backfill.last().map(|row| row.id).unwrap_or(0);

        runtime
            .insert_log(&LogEntry {
                ts: 23,
                ts_nanos: 0,
                level: "INFO".to_string(),
                target: "cli".to_string(),
                message: Some("four".to_string()),
                process_id: Some("thread-1".to_string()),
                process_uuid: None,
                file: Some("main.rs".to_string()),
                line: Some(4),
                module_path: None,
            })
            .await
            .expect("insert newer log");

        let rows = runtime
            .query_logs_after(&LogQuery::default(), last_id, None)
            .await
            .expect("query newer logs");

        let messages = rows
            .iter()
            .map(|row| row.message.as_deref().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(messages, vec!["four"]);

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn tail_backfill_and_poll_advance_cursor_for_live_consumers() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        runtime
            .insert_logs(&[
                LogEntry {
                    ts: 30,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("one".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: None,
                    file: Some("main.rs".to_string()),
                    line: Some(1),
                    module_path: None,
                },
                LogEntry {
                    ts: 31,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("two".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: None,
                    file: Some("main.rs".to_string()),
                    line: Some(2),
                    module_path: None,
                },
            ])
            .await
            .expect("insert initial logs");

        let backfill = runtime
            .tail_backfill(&LogQuery::default(), 1)
            .await
            .expect("tail backfill");
        assert_eq!(backfill.rows.len(), 1);
        assert_eq!(backfill.rows[0].message.as_deref(), Some("two"));

        runtime
            .insert_log(&LogEntry {
                ts: 32,
                ts_nanos: 0,
                level: "INFO".to_string(),
                target: "cli".to_string(),
                message: Some("three".to_string()),
                process_id: Some("thread-1".to_string()),
                process_uuid: None,
                file: Some("main.rs".to_string()),
                line: Some(3),
                module_path: None,
            })
            .await
            .expect("insert follow-up log");

        let polled = runtime
            .tail_poll(&LogQuery::default(), &backfill.cursor, None)
            .await
            .expect("tail poll");
        assert_eq!(polled.rows.len(), 1);
        assert_eq!(polled.rows[0].message.as_deref(), Some("three"));
        assert!(polled.cursor.last_id > backfill.cursor.last_id);

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn related_process_query_includes_latest_processless_companion_logs_only() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        runtime
            .insert_logs(&[
                LogEntry {
                    ts: 40,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("old-thread".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: Some("proc-old".to_string()),
                    file: Some("main.rs".to_string()),
                    line: Some(1),
                    module_path: None,
                },
                LogEntry {
                    ts: 41,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("old-processless".to_string()),
                    process_id: None,
                    process_uuid: Some("proc-old".to_string()),
                    file: Some("main.rs".to_string()),
                    line: Some(2),
                    module_path: None,
                },
                LogEntry {
                    ts: 42,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("new-thread".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: Some("proc-new".to_string()),
                    file: Some("main.rs".to_string()),
                    line: Some(3),
                    module_path: None,
                },
                LogEntry {
                    ts: 43,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("new-processless".to_string()),
                    process_id: None,
                    process_uuid: Some("proc-new".to_string()),
                    file: Some("main.rs".to_string()),
                    line: Some(4),
                    module_path: None,
                },
                LogEntry {
                    ts: 44,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("other-processless".to_string()),
                    process_id: None,
                    process_uuid: Some("proc-other".to_string()),
                    file: Some("main.rs".to_string()),
                    line: Some(5),
                    module_path: None,
                },
            ])
            .await
            .expect("insert scoped logs");

        let rows = runtime
            .query_logs(&LogQuery {
                related_to_process_id: Some("thread-1".to_string()),
                include_related_processless: true,
                ..Default::default()
            })
            .await
            .expect("query related logs");

        let messages = rows
            .iter()
            .map(|row| row.message.as_deref().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            vec!["old-thread", "new-thread", "new-processless"]
        );

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn insert_logs_prunes_old_rows_when_thread_exceeds_size_limit() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        let six_mebibytes = "a".repeat(6 * 1024 * 1024);
        runtime
            .insert_logs(&[
                LogEntry {
                    ts: 1,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some(six_mebibytes.clone()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: Some("proc-1".to_string()),
                    file: Some("main.rs".to_string()),
                    line: Some(1),
                    module_path: Some("mod".to_string()),
                },
                LogEntry {
                    ts: 2,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some(six_mebibytes.clone()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: Some("proc-1".to_string()),
                    file: Some("main.rs".to_string()),
                    line: Some(2),
                    module_path: Some("mod".to_string()),
                },
            ])
            .await
            .expect("insert test logs");

        let rows = runtime
            .query_logs(&LogQuery {
                process_ids: vec!["thread-1".to_string()],
                ..Default::default()
            })
            .await
            .expect("query thread logs");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ts, 2);

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn insert_logs_prunes_single_thread_row_when_it_exceeds_size_limit() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        let eleven_mebibytes = "d".repeat(11 * 1024 * 1024);
        runtime
            .insert_logs(&[LogEntry {
                ts: 1,
                ts_nanos: 0,
                level: "INFO".to_string(),
                target: "cli".to_string(),
                message: Some(eleven_mebibytes),
                process_id: Some("thread-oversized".to_string()),
                process_uuid: Some("proc-1".to_string()),
                file: Some("main.rs".to_string()),
                line: Some(1),
                module_path: Some("mod".to_string()),
            }])
            .await
            .expect("insert test log");

        let rows = runtime
            .query_logs(&LogQuery {
                process_ids: vec!["thread-oversized".to_string()],
                ..Default::default()
            })
            .await
            .expect("query thread logs");

        assert!(rows.is_empty());

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn insert_logs_prunes_processless_rows_per_process_uuid_only() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        let six_mebibytes = "b".repeat(6 * 1024 * 1024);
        runtime
            .insert_logs(&[
                LogEntry {
                    ts: 1,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some(six_mebibytes.clone()),
                    process_id: None,
                    process_uuid: Some("proc-1".to_string()),
                    file: Some("main.rs".to_string()),
                    line: Some(1),
                    module_path: Some("mod".to_string()),
                },
                LogEntry {
                    ts: 2,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some(six_mebibytes.clone()),
                    process_id: None,
                    process_uuid: Some("proc-1".to_string()),
                    file: Some("main.rs".to_string()),
                    line: Some(2),
                    module_path: Some("mod".to_string()),
                },
                LogEntry {
                    ts: 3,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some(six_mebibytes),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: Some("proc-1".to_string()),
                    file: Some("main.rs".to_string()),
                    line: Some(3),
                    module_path: Some("mod".to_string()),
                },
            ])
            .await
            .expect("insert test logs");

        let rows = runtime
            .query_logs(&LogQuery {
                process_ids: vec!["thread-1".to_string()],
                include_processless: true,
                ..Default::default()
            })
            .await
            .expect("query thread and processless logs");

        let mut timestamps: Vec<i64> = rows.into_iter().map(|row| row.ts).collect();
        timestamps.sort_unstable();
        assert_eq!(timestamps, vec![2, 3]);

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn insert_logs_prunes_single_processless_process_row_when_it_exceeds_size_limit() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        let eleven_mebibytes = "e".repeat(11 * 1024 * 1024);
        runtime
            .insert_logs(&[LogEntry {
                ts: 1,
                ts_nanos: 0,
                level: "INFO".to_string(),
                target: "cli".to_string(),
                message: Some(eleven_mebibytes),
                process_id: None,
                process_uuid: Some("proc-oversized".to_string()),
                file: Some("main.rs".to_string()),
                line: Some(1),
                module_path: Some("mod".to_string()),
            }])
            .await
            .expect("insert test log");

        let rows = runtime
            .query_logs(&LogQuery {
                include_processless: true,
                ..Default::default()
            })
            .await
            .expect("query processless logs");

        assert!(rows.is_empty());

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn insert_logs_prunes_processless_rows_with_null_process_uuid() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        let six_mebibytes = "c".repeat(6 * 1024 * 1024);
        runtime
            .insert_logs(&[
                LogEntry {
                    ts: 1,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some(six_mebibytes.clone()),
                    process_id: None,
                    process_uuid: None,
                    file: Some("main.rs".to_string()),
                    line: Some(1),
                    module_path: Some("mod".to_string()),
                },
                LogEntry {
                    ts: 2,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some(six_mebibytes),
                    process_id: None,
                    process_uuid: None,
                    file: Some("main.rs".to_string()),
                    line: Some(2),
                    module_path: Some("mod".to_string()),
                },
                LogEntry {
                    ts: 3,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("small".to_string()),
                    process_id: None,
                    process_uuid: Some("proc-1".to_string()),
                    file: Some("main.rs".to_string()),
                    line: Some(3),
                    module_path: Some("mod".to_string()),
                },
            ])
            .await
            .expect("insert test logs");

        let rows = runtime
            .query_logs(&LogQuery {
                include_processless: true,
                ..Default::default()
            })
            .await
            .expect("query processless logs");

        let mut timestamps: Vec<i64> = rows.into_iter().map(|row| row.ts).collect();
        timestamps.sort_unstable();
        assert_eq!(timestamps, vec![2, 3]);

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn insert_logs_prunes_single_processless_null_process_row_when_it_exceeds_limit() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        let eleven_mebibytes = "f".repeat(11 * 1024 * 1024);
        runtime
            .insert_logs(&[LogEntry {
                ts: 1,
                ts_nanos: 0,
                level: "INFO".to_string(),
                target: "cli".to_string(),
                message: Some(eleven_mebibytes),
                process_id: None,
                process_uuid: None,
                file: Some("main.rs".to_string()),
                line: Some(1),
                module_path: Some("mod".to_string()),
            }])
            .await
            .expect("insert test log");

        let rows = runtime
            .query_logs(&LogQuery {
                include_processless: true,
                ..Default::default()
            })
            .await
            .expect("query processless logs");

        assert!(rows.is_empty());

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn insert_logs_prunes_old_rows_when_thread_exceeds_row_limit() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        let entries: Vec<LogEntry> = (1..=1_001)
            .map(|ts| LogEntry {
                ts,
                ts_nanos: 0,
                level: "INFO".to_string(),
                target: "cli".to_string(),
                message: Some(format!("thread-row-{ts}")),
                process_id: Some("thread-row-limit".to_string()),
                process_uuid: Some("proc-1".to_string()),
                file: Some("main.rs".to_string()),
                line: Some(ts),
                module_path: Some("mod".to_string()),
            })
            .collect();
        runtime
            .insert_logs(&entries)
            .await
            .expect("insert test logs");

        let rows = runtime
            .query_logs(&LogQuery {
                process_ids: vec!["thread-row-limit".to_string()],
                ..Default::default()
            })
            .await
            .expect("query thread logs");

        let timestamps: Vec<i64> = rows.into_iter().map(|row| row.ts).collect();
        assert_eq!(timestamps.len(), 1_000);
        assert_eq!(timestamps.first().copied(), Some(2));
        assert_eq!(timestamps.last().copied(), Some(1_001));

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn insert_logs_prunes_old_processless_rows_when_process_exceeds_row_limit() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        let entries: Vec<LogEntry> = (1..=1_001)
            .map(|ts| LogEntry {
                ts,
                ts_nanos: 0,
                level: "INFO".to_string(),
                target: "cli".to_string(),
                message: Some(format!("process-row-{ts}")),
                process_id: None,
                process_uuid: Some("proc-row-limit".to_string()),
                file: Some("main.rs".to_string()),
                line: Some(ts),
                module_path: Some("mod".to_string()),
            })
            .collect();
        runtime
            .insert_logs(&entries)
            .await
            .expect("insert test logs");

        let rows = runtime
            .query_logs(&LogQuery {
                include_processless: true,
                ..Default::default()
            })
            .await
            .expect("query processless logs");

        let timestamps: Vec<i64> = rows
            .into_iter()
            .filter(|row| row.process_uuid.as_deref() == Some("proc-row-limit"))
            .map(|row| row.ts)
            .collect();
        assert_eq!(timestamps.len(), 1_000);
        assert_eq!(timestamps.first().copied(), Some(2));
        assert_eq!(timestamps.last().copied(), Some(1_001));

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn insert_logs_prunes_old_processless_null_process_rows_when_row_limit_exceeded() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        let entries: Vec<LogEntry> = (1..=1_001)
            .map(|ts| LogEntry {
                ts,
                ts_nanos: 0,
                level: "INFO".to_string(),
                target: "cli".to_string(),
                message: Some(format!("null-process-row-{ts}")),
                process_id: None,
                process_uuid: None,
                file: Some("main.rs".to_string()),
                line: Some(ts),
                module_path: Some("mod".to_string()),
            })
            .collect();
        runtime
            .insert_logs(&entries)
            .await
            .expect("insert test logs");

        let rows = runtime
            .query_logs(&LogQuery {
                include_processless: true,
                ..Default::default()
            })
            .await
            .expect("query processless logs");

        let timestamps: Vec<i64> = rows
            .into_iter()
            .filter(|row| row.process_uuid.is_none())
            .map(|row| row.ts)
            .collect();
        assert_eq!(timestamps.len(), 1_000);
        assert_eq!(timestamps.first().copied(), Some(2));
        assert_eq!(timestamps.last().copied(), Some(1_001));

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn query_feedback_logs_returns_newest_lines_within_limit_in_order() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        runtime
            .insert_logs(&[
                LogEntry {
                    ts: 1,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("alpha".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: Some("proc-1".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
                LogEntry {
                    ts: 2,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("bravo".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: Some("proc-1".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
                LogEntry {
                    ts: 3,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("charlie".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: Some("proc-1".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
            ])
            .await
            .expect("insert test logs");

        let bytes = runtime
            .query_feedback_logs("thread-1")
            .await
            .expect("query feedback logs");

        assert_eq!(
            String::from_utf8(bytes).expect("valid utf-8"),
            "1970-01-01T00:00:01.000000Z  INFO alpha\n1970-01-01T00:00:02.000000Z  INFO bravo\n1970-01-01T00:00:03.000000Z  INFO charlie\n"
        );

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn query_feedback_logs_excludes_oversized_newest_row() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");
        let eleven_mebibytes = "z".repeat(11 * 1024 * 1024);

        runtime
            .insert_logs(&[
                LogEntry {
                    ts: 1,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("small".to_string()),
                    process_id: Some("thread-oversized".to_string()),
                    process_uuid: Some("proc-1".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
                LogEntry {
                    ts: 2,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some(eleven_mebibytes),
                    process_id: Some("thread-oversized".to_string()),
                    process_uuid: Some("proc-1".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
            ])
            .await
            .expect("insert test logs");

        let bytes = runtime
            .query_feedback_logs("thread-oversized")
            .await
            .expect("query feedback logs");

        assert_eq!(bytes, Vec::<u8>::new());

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn query_feedback_logs_includes_processless_rows_from_same_process() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        runtime
            .insert_logs(&[
                LogEntry {
                    ts: 1,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("processless-before".to_string()),
                    process_id: None,
                    process_uuid: Some("proc-1".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
                LogEntry {
                    ts: 2,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("process-scoped".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: Some("proc-1".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
                LogEntry {
                    ts: 3,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("processless-after".to_string()),
                    process_id: None,
                    process_uuid: Some("proc-1".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
                LogEntry {
                    ts: 4,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("other-process-processless".to_string()),
                    process_id: None,
                    process_uuid: Some("proc-2".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
            ])
            .await
            .expect("insert test logs");

        let bytes = runtime
            .query_feedback_logs("thread-1")
            .await
            .expect("query feedback logs");

        assert_eq!(
            String::from_utf8(bytes).expect("valid utf-8"),
            "1970-01-01T00:00:01.000000Z  INFO processless-before\n1970-01-01T00:00:02.000000Z  INFO process-scoped\n1970-01-01T00:00:03.000000Z  INFO processless-after\n"
        );

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn query_feedback_logs_excludes_processless_rows_from_prior_processes() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");

        runtime
            .insert_logs(&[
                LogEntry {
                    ts: 1,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("old-process-processless".to_string()),
                    process_id: None,
                    process_uuid: Some("proc-old".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
                LogEntry {
                    ts: 2,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("old-process-thread".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: Some("proc-old".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
                LogEntry {
                    ts: 3,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("new-process-thread".to_string()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: Some("proc-new".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
                LogEntry {
                    ts: 4,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some("new-process-processless".to_string()),
                    process_id: None,
                    process_uuid: Some("proc-new".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
            ])
            .await
            .expect("insert test logs");

        let bytes = runtime
            .query_feedback_logs("thread-1")
            .await
            .expect("query feedback logs");

        assert_eq!(
            String::from_utf8(bytes).expect("valid utf-8"),
            "1970-01-01T00:00:02.000000Z  INFO old-process-thread\n1970-01-01T00:00:03.000000Z  INFO new-process-thread\n1970-01-01T00:00:04.000000Z  INFO new-process-processless\n"
        );

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }

    #[tokio::test]
    async fn query_feedback_logs_keeps_newest_suffix_across_process_and_processless_logs() {
        let chaos_home = unique_temp_dir();
        let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
            .await
            .expect("initialize runtime");
        let thread_marker = "process-scoped-oldest";
        let processless_older_marker = "processless-older";
        let processless_newer_marker = "processless-newer";
        let five_mebibytes = format!("{processless_older_marker} {}", "a".repeat(5 * 1024 * 1024));
        let four_and_half_mebibytes = format!(
            "{processless_newer_marker} {}",
            "b".repeat((9 * 1024 * 1024) / 2)
        );
        let one_mebibyte = format!("{thread_marker} {}", "c".repeat(1024 * 1024));

        runtime
            .insert_logs(&[
                LogEntry {
                    ts: 1,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some(one_mebibyte.clone()),
                    process_id: Some("thread-1".to_string()),
                    process_uuid: Some("proc-1".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
                LogEntry {
                    ts: 2,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some(five_mebibytes),
                    process_id: None,
                    process_uuid: Some("proc-1".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
                LogEntry {
                    ts: 3,
                    ts_nanos: 0,
                    level: "INFO".to_string(),
                    target: "cli".to_string(),
                    message: Some(four_and_half_mebibytes),
                    process_id: None,
                    process_uuid: Some("proc-1".to_string()),
                    file: None,
                    line: None,
                    module_path: None,
                },
            ])
            .await
            .expect("insert test logs");

        let bytes = runtime
            .query_feedback_logs("thread-1")
            .await
            .expect("query feedback logs");
        let logs = String::from_utf8(bytes).expect("valid utf-8");

        assert!(!logs.contains(thread_marker));
        assert!(logs.contains(processless_older_marker));
        assert!(logs.contains(processless_newer_marker));
        assert_eq!(logs.matches('\n').count(), 2);

        let _ = tokio::fs::remove_dir_all(chaos_home).await;
    }
}
