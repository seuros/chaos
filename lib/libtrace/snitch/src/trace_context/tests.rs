use super::context_from_trace_headers;
use super::context_from_w3c_trace_context;
use super::current_span_trace_id;
use chaos_ipc::protocol::W3cTraceContext;
use pretty_assertions::assert_eq;
use rama::telemetry::opentelemetry::sdk::trace::SdkTracerProvider;
use rama::telemetry::opentelemetry::trace::SpanId;
use rama::telemetry::opentelemetry::trace::TraceContextExt;
use rama::telemetry::opentelemetry::trace::TraceId;
use rama::telemetry::opentelemetry::trace::TracerProvider as _;
use tracing::trace_span;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[test]
fn parses_valid_w3c_trace_context() {
    let trace_id = "00000000000000000000000000000001";
    let span_id = "0000000000000002";
    let context = context_from_w3c_trace_context(&W3cTraceContext {
        traceparent: Some(format!("00-{trace_id}-{span_id}-01")),
        tracestate: None,
    })
    .expect("trace context");

    let span = context.span();
    let span_context = span.span_context();
    assert_eq!(
        span_context.trace_id(),
        TraceId::from_hex(trace_id).unwrap()
    );
    assert_eq!(span_context.span_id(), SpanId::from_hex(span_id).unwrap());
    assert!(span_context.is_remote());
}

#[test]
fn invalid_traceparent_returns_none() {
    assert!(context_from_trace_headers(Some("not-a-traceparent"), None).is_none());
}

#[test]
fn missing_traceparent_returns_none() {
    assert!(
        context_from_w3c_trace_context(&W3cTraceContext {
            traceparent: None,
            tracestate: Some("vendor=value".to_string()),
        })
        .is_none()
    );
}

#[test]
fn current_span_trace_id_returns_hex_trace_id() {
    let provider = SdkTracerProvider::builder().build();
    let tracer = provider.tracer("chaos-otel-tests");
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
    let _guard = subscriber.set_default();

    let span = trace_span!("test_span");
    let _entered = span.enter();
    let trace_id = current_span_trace_id().expect("trace id");

    assert_eq!(trace_id.len(), 32);
    assert!(trace_id.chars().all(|ch| ch.is_ascii_hexdigit()));
    assert_ne!(trace_id, "00000000000000000000000000000000");
}
