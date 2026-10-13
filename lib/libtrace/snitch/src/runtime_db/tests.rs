use super::*;
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
