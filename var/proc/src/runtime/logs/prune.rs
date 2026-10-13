//! Shared log retention: one body for SQLite and PostgreSQL. Derived tables
//! carry an alias because PostgreSQL requires one.
macro_rules! logs_prune_backend {
    ($runtime:ty, $database:ty, $connection:ty) => {
        impl $runtime {
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
        pub(super) async fn prune_logs_after_insert(
            &self,
            entries: &[LogEntry],
            tx: &mut $connection,
        ) -> anyhow::Result<()> {
            let process_ids: BTreeSet<&str> = entries
                .iter()
                .filter_map(|entry| entry.process_id.as_deref())
                .collect();
            if !process_ids.is_empty() {
                // Cheap precheck: only run the heavier window-function prune for
                // processes that are currently above the cap.
                let mut over_limit_threads_query =
                    QueryBuilder::<$database>::new(
                        "SELECT process_id FROM logs WHERE process_id IN (",
                    );
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
                    let mut prune_threads = QueryBuilder::<$database>::new(
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
        ) AS ranked
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
                let mut over_limit_processes_query = QueryBuilder::<$database>::new(
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
                    let mut prune_processless_process_logs = QueryBuilder::<$database>::new(
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
        ) AS ranked
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
                let mut null_process_usage_query = QueryBuilder::<$database>::new("SELECT SUM(");
                null_process_usage_query.push("estimated_bytes");
                null_process_usage_query.push(
                    ") AS total_bytes, COUNT(*) AS row_count FROM logs \
                     WHERE process_id IS NULL AND process_uuid IS NULL",
                );
                let null_process_usage =
                    null_process_usage_query.build().fetch_one(&mut *tx).await?;
                let total_null_process_bytes: Option<i64> =
                    null_process_usage.try_get("total_bytes")?;
                let null_process_row_count: i64 = null_process_usage.try_get("row_count")?;

                if total_null_process_bytes.unwrap_or(0) > LOG_PARTITION_SIZE_LIMIT_BYTES
                    || null_process_row_count > LOG_PARTITION_ROW_LIMIT
                {
                    let mut prune_processless_null_process_logs = QueryBuilder::<$database>::new(
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
        ) AS ranked
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
        }
    };
}
pub(super) use logs_prune_backend;
