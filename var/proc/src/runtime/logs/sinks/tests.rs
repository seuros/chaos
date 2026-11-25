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
async fn recent_logs_and_query_logs_after_return_ordered_batches() {
    fn messages(rows: &[crate::LogRow]) -> Vec<&str> {
        rows.iter()
            .map(|row| row.message.as_deref().expect("log message"))
            .collect()
    }

    let chaos_home = unique_temp_dir();
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");
    let query = LogQuery::default();
    let entry = |message: &str| LogEntry {
        ts: 10,
        ts_nanos: 0,
        level: "INFO".into(),
        target: "cli".into(),
        message: Some(message.into()),
        process_id: Some("thread-1".into()),
        process_uuid: None,
        module_path: None,
        file: None,
        line: None,
    };
    runtime
        .insert_logs(&["one", "two", "three"].map(entry))
        .await
        .expect("insert initial logs");
    let backfill = runtime
        .recent_logs(&query, 2)
        .await
        .expect("query recent logs");
    assert_eq!(messages(&backfill), ["two", "three"]);
    let last_id = backfill.last().expect("backfill cursor").id;

    runtime
        .insert_logs(&["four", "five"].map(entry))
        .await
        .expect("insert newer logs");
    let rows = runtime
        .query_logs_after(&query, last_id, None)
        .await
        .expect("query logs after backfill cursor");
    assert_eq!(messages(&rows), ["four", "five"]);

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn log_search_filters_by_substring_and_tail_advances_its_cursor() {
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
                message: Some("alpha".to_string()),
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
                message: Some("alphabet".to_string()),
                process_id: Some("thread-1".to_string()),
                process_uuid: None,
                file: Some("main.rs".to_string()),
                line: Some(2),
                module_path: None,
            },
        ])
        .await
        .expect("insert initial logs");

    let rows = runtime
        .query_logs(&LogQuery {
            search: Some("phab".into()),
            ..Default::default()
        })
        .await
        .expect("query by interior substring");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].message.as_deref(), Some("alphabet"));

    let backfill = runtime
        .tail_backfill(&LogQuery::default(), 1)
        .await
        .expect("tail backfill");
    assert_eq!(backfill.rows.len(), 1);
    assert_eq!(backfill.rows[0].message.as_deref(), Some("alphabet"));

    let zero_backfill = runtime
        .tail_backfill(&LogQuery::default(), 0)
        .await
        .expect("zero-limit tail backfill");
    assert!(zero_backfill.rows.is_empty());
    assert_eq!(zero_backfill.cursor, backfill.cursor);

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

    let empty_poll = runtime
        .tail_poll(&LogQuery::default(), &polled.cursor, None)
        .await
        .expect("tail poll without newer logs");
    assert!(empty_poll.rows.is_empty());
    assert_eq!(empty_poll.cursor, polled.cursor);

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
