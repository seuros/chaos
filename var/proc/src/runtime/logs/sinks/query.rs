use super::super::super::{
    LOG_PARTITION_SIZE_LIMIT_BYTES, LogQuery, LogRow, QueryBuilder, Row, Sqlite, StateRuntime,
};
use super::super::formatter::push_log_filters;

impl StateRuntime {
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
