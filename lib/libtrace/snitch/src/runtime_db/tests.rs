use super::*;
use crate::tracing;
use crate::tracing::subscriber as tracing_subscriber;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::test]
async fn flush_persists_logs_for_query() {
    let chaos_home = std::env::temp_dir().join(format!("chaos-state-log-db-{}", Uuid::new_v4()));
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");
    let layer = start_runtime_db_layer(runtime.clone());

    let guard = tracing_subscriber::registry()
        .with(
            layer
                .clone()
                .with_filter(Targets::new().with_default(tracing::Level::TRACE)),
        )
        .set_default();

    tracing::info!("buffered-log");

    layer.flush().await;
    drop(guard);

    let after_flush = runtime
        .query_logs(&chaos_proc::LogQuery::default())
        .await
        .expect("query logs after flush");
    assert_eq!(after_flush.len(), 1);
    assert_eq!(after_flush[0].message.as_deref(), Some("buffered-log"));

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

async fn wait_for_logs(
    runtime: &StateRuntime,
    expected: usize,
    budget: Duration,
) -> Vec<chaos_proc::LogRow> {
    let started = std::time::Instant::now();
    loop {
        let logs = runtime
            .query_logs(&chaos_proc::LogQuery::default())
            .await
            .expect("query logs");
        if logs.len() >= expected || started.elapsed() > budget {
            return logs;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn reaching_batch_size_flushes_before_the_deadline() {
    let chaos_home = std::env::temp_dir().join(format!("chaos-state-log-size-{}", Uuid::new_v4()));
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");
    let layer = start_runtime_db_layer(runtime.clone());

    let guard = tracing_subscriber::registry()
        .with(
            layer
                .clone()
                .with_filter(Targets::new().with_default(tracing::Level::TRACE)),
        )
        .set_default();

    let started = std::time::Instant::now();
    for index in 0..LOG_BATCH_SIZE.get() {
        tracing::info!(index, "sized-log");
    }
    let logs = wait_for_logs(
        &runtime,
        LOG_BATCH_SIZE.get() as usize,
        LOG_FLUSH_INTERVAL / 2,
    )
    .await;
    let elapsed = started.elapsed();
    drop(guard);

    assert_eq!(logs.len(), LOG_BATCH_SIZE.get() as usize);
    assert!(
        elapsed < LOG_FLUSH_INTERVAL,
        "size flush must not wait for the deadline: {elapsed:?}"
    );

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

#[tokio::test]
async fn deadline_flushes_a_partial_batch() {
    let chaos_home =
        std::env::temp_dir().join(format!("chaos-state-log-deadline-{}", Uuid::new_v4()));
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");
    let layer = start_runtime_db_layer(runtime.clone());

    let guard = tracing_subscriber::registry()
        .with(
            layer
                .clone()
                .with_filter(Targets::new().with_default(tracing::Level::TRACE)),
        )
        .set_default();

    tracing::info!("partial-log");
    let logs = wait_for_logs(&runtime, 1, LOG_FLUSH_INTERVAL * 3).await;
    drop(guard);

    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].message.as_deref(), Some("partial-log"));

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}
