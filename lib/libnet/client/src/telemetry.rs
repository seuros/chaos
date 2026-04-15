use crate::error::TransportError;
use rama::http::HeaderMap;
use rama::http::HeaderName;
use rama::http::HeaderValue;
use rama::http::StatusCode;
use rama::telemetry::opentelemetry::global;
use rama::telemetry::opentelemetry::propagation::Injector;
use std::time::Duration;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Inject the active OpenTelemetry span context into outgoing HTTP headers.
///
/// Both the high-level `ChaosHttpClient` and the lower-level `RamaTransport`
/// rely on the same propagation logic; sharing this helper keeps trace
/// headers consistent regardless of which client path a caller takes.
pub(crate) fn inject_trace_headers(headers: &mut HeaderMap) {
    struct HeaderMapInjector<'a>(&'a mut HeaderMap);

    impl Injector for HeaderMapInjector<'_> {
        fn set(&mut self, key: &str, value: String) {
            if let (Ok(name), Ok(val)) = (
                HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(&value),
            ) {
                self.0.insert(name, val);
            }
        }
    }

    global::get_text_map_propagator(|prop| {
        prop.inject_context(&Span::current().context(), &mut HeaderMapInjector(headers));
    });
}

/// API specific telemetry.
pub trait RequestTelemetry: Send + Sync {
    fn on_request(
        &self,
        attempt: u64,
        status: Option<StatusCode>,
        error: Option<&TransportError>,
        duration: Duration,
    );
}
