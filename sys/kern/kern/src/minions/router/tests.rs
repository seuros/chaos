//! Focused unit tests for the pieces of this module that aren't
//! covered end-to-end by `minions::control::tests` (which exercise
//! Spawn / Resume / Fork through a real `ProcessTable`).
//!
//! Adapter-level trace carrier propagation and full-mailbox
//! backpressure are already covered by `chaos_traits::router::tests`;
//! we don't duplicate those here.
use super::*;
use rama::telemetry::opentelemetry::sdk::trace::SdkTracerProvider;
use rama::telemetry::opentelemetry::trace::TraceContextExt;
use rama::telemetry::opentelemetry::trace::TracerProvider as _;
use std::sync::Once;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::prelude::*;

fn init_tracing() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("chaos-router-tests");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

/// The router body's instrumentation span must re-parent onto the
/// caller's W3C carrier so OTel exports see one logical trace
/// across the mailbox hop. Without this, `Spawn` / `Resume` /
/// `Fork` bodies would appear as independent traces and the
/// submission → router → state-mutation chain would fragment.
#[test]
fn span_from_packet_path_reparents_to_w3c_carrier() {
    init_tracing();
    let carrier = W3cTraceContext {
        traceparent: Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".into()),
        tracestate: None,
    };
    let span = span_from_packet_path(Some(&carrier));
    let trace_id = span.context().span().span_context().trace_id().to_string();
    assert_eq!(trace_id, "0af7651916cd43dd8448eb211c80319c");
}
