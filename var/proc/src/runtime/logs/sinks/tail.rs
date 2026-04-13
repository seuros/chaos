use super::super::super::{LogQuery, LogRow, StateRuntime};
use crate::LogTailBatch;
use crate::LogTailCursor;

impl StateRuntime {
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
}
