use super::super::super::{LogQuery, LogRow, QueryBuilder, Row, Sqlite, StateRuntime};
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
