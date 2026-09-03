use super::super::{LogEntry, LogQuery, LogRow, PostgresRuntime, QueryBuilder, RuntimeDbHandle};
use crate::{LogTailBatch, LogTailCursor};
use sqlx::{Postgres, Row};

impl RuntimeDbHandle {
    pub async fn insert_logs(&self, entries: &[LogEntry]) -> anyhow::Result<()> {
        match self {
            Self::Postgres(runtime) => runtime.insert_logs(entries).await,
            Self::Sqlite(runtime) => runtime.insert_logs(entries).await,
        }
    }

    pub async fn delete_logs_before(&self, cutoff_ts: i64) -> anyhow::Result<u64> {
        match self {
            Self::Postgres(runtime) => runtime.delete_logs_before(cutoff_ts).await,
            Self::Sqlite(runtime) => runtime.delete_logs_before(cutoff_ts).await,
        }
    }

    pub async fn tail_backfill(
        &self,
        query: &LogQuery,
        limit: usize,
    ) -> anyhow::Result<LogTailBatch> {
        match self {
            Self::Postgres(runtime) => runtime.tail_backfill(query, limit).await,
            Self::Sqlite(runtime) => runtime.tail_backfill(query, limit).await,
        }
    }
}

impl PostgresRuntime {
    async fn insert_logs(&self, entries: &[LogEntry]) -> anyhow::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        let postgres_entries = entries
            .iter()
            .map(|entry| {
                let ts_nanos = i32::try_from(entry.ts_nanos)
                    .map_err(|_| anyhow::anyhow!("log ts_nanos is outside PostgreSQL INTEGER"))?;
                let line = entry
                    .line
                    .map(i32::try_from)
                    .transpose()
                    .map_err(|_| anyhow::anyhow!("log line is outside PostgreSQL INTEGER"))?;
                Ok((entry, ts_nanos, line))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        let mut builder = QueryBuilder::<Postgres>::new(
            "INSERT INTO logs (ts, ts_nanos, level, target, message, process_id, process_uuid, module_path, file, line, estimated_bytes) ",
        );
        builder.push_values(postgres_entries, |mut row, (entry, ts_nanos, line)| {
            let estimated_bytes = entry.message.as_ref().map_or(0, String::len) as i64
                + entry.level.len() as i64
                + entry.target.len() as i64
                + entry.module_path.as_ref().map_or(0, String::len) as i64
                + entry.file.as_ref().map_or(0, String::len) as i64;
            row.push_bind(entry.ts)
                .push_bind(ts_nanos)
                .push_bind(&entry.level)
                .push_bind(&entry.target)
                .push_bind(&entry.message)
                .push_bind(&entry.process_id)
                .push_bind(&entry.process_uuid)
                .push_bind(&entry.module_path)
                .push_bind(&entry.file)
                .push_bind(line)
                .push_bind(estimated_bytes);
        });
        builder.build().execute(&self.pool).await?;
        Ok(())
    }

    async fn delete_logs_before(&self, cutoff_ts: i64) -> anyhow::Result<u64> {
        let result = sqlx::query("DELETE FROM logs WHERE ts < $1")
            .bind(cutoff_ts)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    async fn tail_backfill(&self, query: &LogQuery, limit: usize) -> anyhow::Result<LogTailBatch> {
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

    async fn recent_logs(&self, query: &LogQuery, limit: usize) -> anyhow::Result<Vec<LogRow>> {
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

    async fn query_logs(&self, query: &LogQuery) -> anyhow::Result<Vec<LogRow>> {
        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id, ts, ts_nanos::BIGINT AS ts_nanos, level, target, message, process_id, process_uuid, file, line::BIGINT AS line FROM logs WHERE 1 = 1",
        );
        push_log_filters(&mut builder, query);
        builder.push(if query.descending {
            " ORDER BY id DESC"
        } else {
            " ORDER BY id ASC"
        });
        if let Some(limit) = query.limit {
            builder.push(" LIMIT ").push_bind(limit as i64);
        }
        Ok(builder
            .build_query_as::<LogRow>()
            .fetch_all(&self.pool)
            .await?)
    }

    async fn max_log_id(&self, query: &LogQuery) -> anyhow::Result<i64> {
        let mut builder =
            QueryBuilder::<Postgres>::new("SELECT MAX(id) AS max_id FROM logs WHERE 1 = 1");
        push_log_filters(&mut builder, query);
        let row = builder.build().fetch_one(&self.pool).await?;
        Ok(row.try_get::<Option<i64>, _>("max_id")?.unwrap_or(0))
    }
}

fn push_log_filters(builder: &mut QueryBuilder<Postgres>, query: &LogQuery) {
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
        builder.push(" AND (process_id = ").push_bind(process_id);
        if query.include_related_processless {
            builder.push(" OR (process_id IS NULL AND process_uuid IN (");
            builder
                .push("SELECT process_uuid FROM logs WHERE process_id = ")
                .push_bind(process_id)
                .push(
                    " AND process_uuid IS NOT NULL ORDER BY ts DESC, ts_nanos DESC, id DESC LIMIT 1",
                );
            builder.push("))");
        }
        builder.push(")");
    } else if !query.process_ids.is_empty() || query.include_processless {
        builder.push(" AND (");
        let mut needs_or = false;
        for process_id in &query.process_ids {
            if needs_or {
                builder.push(" OR ");
            }
            builder.push("process_id = ").push_bind(process_id);
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
    if let Some(after_id) = query.after_id {
        builder.push(" AND id > ").push_bind(after_id);
    }
    if let Some(search) = query.search.as_ref() {
        builder
            .push(" AND STRPOS(message, ")
            .push_bind(search)
            .push(") > 0");
    }
}

fn push_like_filters(builder: &mut QueryBuilder<Postgres>, column: &str, filters: &[String]) {
    if filters.is_empty() {
        return;
    }
    builder.push(" AND (");
    for (index, filter) in filters.iter().enumerate() {
        if index > 0 {
            builder.push(" OR ");
        }
        builder
            .push(column)
            .push(" LIKE '%' || ")
            .push_bind(filter)
            .push(" || '%'");
    }
    builder.push(")");
}
