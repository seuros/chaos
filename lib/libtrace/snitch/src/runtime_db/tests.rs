use super::*;
use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::time::Instant;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Clone, Default)]
struct SharedWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl SharedWriter {
    fn snapshot(&self) -> String {
        String::from_utf8(self.bytes.lock().expect("writer mutex poisoned").clone())
            .expect("valid utf-8")
    }
}

struct SharedWriterGuard {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl<'a> MakeWriter<'a> for SharedWriter {
    type Writer = SharedWriterGuard;

    fn make_writer(&'a self) -> Self::Writer {
        SharedWriterGuard {
            bytes: Arc::clone(&self.bytes),
        }
    }
}

impl io::Write for SharedWriterGuard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes
            .lock()
            .expect("writer mutex poisoned")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn sqlite_feedback_logs_match_feedback_formatter_shape() {
    let chaos_home = std::env::temp_dir().join(format!("chaos-state-log-db-{}", Uuid::new_v4()));
    let runtime = StateRuntime::init(chaos_home.clone(), "test-provider".to_string())
        .await
        .expect("initialize runtime");
    let writer = SharedWriter::default();

    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer.clone())
                .without_time()
                .with_ansi(false)
                .with_target(false)
                .with_filter(Targets::new().with_default(tracing::Level::TRACE)),
        )
        .with(
            start_runtime_db_layer(runtime.clone())
                .with_filter(Targets::new().with_default(tracing::Level::TRACE)),
        );
    let guard = subscriber.set_default();

    tracing::trace!("processless-before");
    tracing::info_span!("feedback-thread", process_id = "thread-1").in_scope(|| {
        tracing::info!("process-scoped");
    });
    tracing::debug!("processless-after");

    drop(guard);

    let feedback_logs = writer
        .snapshot()
        .replace("feedback-thread{process_id=\"thread-1\"}: ", "");
    let strip_sqlite_timestamp = |logs: &str| {
        logs.lines()
            .map(|line| {
                line.split_once(' ')
                    .map_or_else(|| line.to_string(), |(_, rest)| rest.to_string())
            })
            .collect::<Vec<_>>()
    };
    let feedback_lines = feedback_logs
        .lines()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let sqlite_logs = String::from_utf8(
            runtime
                .query_feedback_logs("thread-1")
                .await
                .expect("query feedback logs"),
        )
        .expect("valid utf-8");
        if strip_sqlite_timestamp(&sqlite_logs) == feedback_lines {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "sqlite feedback logs did not match feedback formatter output before timeout\nsqlite:\n{sqlite_logs}\nfeedback:\n{feedback_logs}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let _ = tokio::fs::remove_dir_all(chaos_home).await;
}

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
