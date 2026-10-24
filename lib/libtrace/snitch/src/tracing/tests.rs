use super::*;
use rama::telemetry::opentelemetry::sdk::trace::InMemorySpanExporter;
use rama::telemetry::opentelemetry::sdk::trace::SdkTracerProvider;
use rama::telemetry::opentelemetry::trace::TraceContextExt;
use rama::telemetry::opentelemetry::trace::TracerProvider as _;
use std::io::Write;
use std::sync::{Arc, Mutex};
use subscriber::layer::SubscriberExt;
use subscriber::util::SubscriberInitExt;

#[test]
fn reexports_preserve_span_parent_and_fields() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber =
        subscriber::registry().with(layer().with_tracer(provider.tracer("snitch-facade-tests")));
    let _guard = subscriber.set_default();

    let parent = info_span!("parent");
    let parent_context = parent.context();
    let parent_span_context = parent_context.span().span_context().clone();
    {
        let child = span!(
            target: "chaos_snitch::tracing::tests",
            parent: &parent,
            Level::INFO,
            "child",
            operation = "facade",
        );
        let child_context = child.context();
        assert_eq!(
            child_context.span().span_context().trace_id(),
            parent_span_context.trace_id()
        );
    }
    drop(parent);
    provider.force_flush().expect("flush spans");

    let spans = exporter.get_finished_spans().expect("finished spans");
    let child = spans
        .iter()
        .find(|span| span.name == "child")
        .expect("child");
    assert_eq!(child.parent_span_id, parent_span_context.span_id());
    assert!(
        child
            .attributes
            .iter()
            .any(|attribute| attribute.key.as_str() == "operation"
                && attribute.value.as_str() == "facade")
    );
}

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("output").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn reexports_filter_and_flush_non_blocking_logs() {
    let output = Output::default();
    let (writer, worker_guard) = appender::non_blocking(output.clone());
    let subscriber = subscriber::fmt()
        .with_env_filter(subscriber::EnvFilter::new("info"))
        .with_writer(writer)
        .without_time()
        .with_ansi(false)
        .finish();
    {
        let _guard = subscriber.set_default();
        debug!("hidden debug event");
        info!(operation = "facade", "visible info event");
    }
    drop(worker_guard);

    let bytes = output.0.lock().expect("output");
    let text = std::str::from_utf8(&bytes).expect("utf8 logs");
    assert!(text.contains("visible info event"));
    assert!(text.contains("operation=\"facade\""));
    assert!(!text.contains("hidden debug event"));
}
