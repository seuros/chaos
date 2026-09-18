use super::*;
use rama::telemetry::opentelemetry::global;
use rama::telemetry::opentelemetry::propagation::Extractor;
use rama::telemetry::opentelemetry::propagation::TextMapPropagator;
use rama::telemetry::opentelemetry::sdk::propagation::TraceContextPropagator;
use rama::telemetry::opentelemetry::sdk::trace::SdkTracerProvider;
use rama::telemetry::opentelemetry::trace::TraceContextExt;
use rama::telemetry::opentelemetry::trace::TracerProvider;
use tracing::trace_span;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[test]
fn inject_trace_headers_uses_current_span_context() {
    global::set_text_map_propagator(TraceContextPropagator::new());

    let provider = SdkTracerProvider::builder().build();
    let tracer = provider.tracer("test-tracer");
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
    let _guard = subscriber.set_default();

    let span = trace_span!("client_request");
    let _entered = span.enter();
    let span_context = span.context().span().span_context().clone();

    let mut headers = HeaderMap::new();
    inject_trace_headers(&mut headers);

    let extractor = HeaderMapExtractor(&headers);
    let extracted = TraceContextPropagator::new().extract(&extractor);
    let extracted_span = extracted.span();
    let extracted_context = extracted_span.span_context();

    assert!(extracted_context.is_valid());
    assert_eq!(extracted_context.trace_id(), span_context.trace_id());
    assert_eq!(extracted_context.span_id(), span_context.span_id());
}

struct HeaderMapExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderMapExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(HeaderName::as_str).collect()
    }
}
