mod query;
mod tail;
mod write;

#[cfg(test)]
mod tests {
    use super::super::super::StateRuntime;
    use crate::LogEntry;
    use crate::LogQuery;
    use crate::runtime::test_support::unique_temp_dir;
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
