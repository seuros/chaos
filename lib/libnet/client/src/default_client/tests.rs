use super::*;
use rama::error::BoxError;
use rama::error::extra::OpaqueError;
use rama::service::service_fn;
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

#[tokio::test(start_paused = true)]
async fn timeout_covers_headers_and_waiting_for_the_client_lock() {
    let service = service_fn(|_: rama::http::Request| {
        std::future::pending::<Result<rama::http::Response, OpaqueError>>()
    })
    .boxed();
    let client = ChaosHttpClient::new(service);
    let timeout = Duration::from_millis(60);

    let result = client
        .get("http://example.test")
        .timeout(timeout)
        .send()
        .await;
    assert!(matches!(result, Err(ChaosClientError::Timeout)));

    let _lock = client.inner.lock().await;
    let result = client
        .get("http://example.test")
        .timeout(timeout)
        .execute()
        .await;
    assert!(matches!(result, Err(TransportError::Timeout)));
}

#[tokio::test(start_paused = true)]
async fn body_read_shares_the_send_deadline_instead_of_resetting_it() {
    let service = service_fn(|_: rama::http::Request| async {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let body = Body::from_stream(futures::stream::once(async {
            tokio::time::sleep(Duration::from_millis(40)).await;
            Ok::<_, BoxError>(Bytes::from_static(b"{}"))
        }));
        Ok::<_, OpaqueError>(rama::http::Response::new(body))
    })
    .boxed();
    let client = ChaosHttpClient::new(service);
    let start = Instant::now();
    let response = client
        .get("http://example.test")
        .timeout(Duration::from_millis(60))
        .send()
        .await
        .expect("headers arrive before the deadline");
    assert!(matches!(
        response.bytes().await,
        Err(ChaosClientError::Timeout)
    ));
    assert_eq!(start.elapsed(), Duration::from_millis(60));
}

#[tokio::test]
async fn execute_preserves_status_headers_and_body_for_success_and_failure() {
    for status in [200, 429] {
        let service = service_fn(move |_: rama::http::Request| async move {
            Ok::<_, OpaqueError>(
                rama::http::Response::builder()
                    .status(status)
                    .header("x-request-id", "request-1")
                    .body(Body::from("response text"))
                    .expect("valid response"),
            )
        })
        .boxed();
        let result = ChaosHttpClient::new(service)
            .get("http://example.test")
            .execute()
            .await;
        match result {
            Ok(response) => {
                assert_eq!(status, 200);
                assert_eq!(response.status.as_u16(), status);
                assert_eq!(response.headers["x-request-id"], "request-1");
                assert_eq!(response.body.as_ref(), b"response text");
            }
            Err(TransportError::Http {
                status: actual,
                headers,
                body,
                url,
            }) => {
                assert_eq!(status, 429);
                assert_eq!(actual.as_u16(), status);
                assert_eq!(
                    headers.expect("response headers")["x-request-id"],
                    "request-1"
                );
                assert_eq!(body.as_deref(), Some("response text"));
                assert_eq!(url.as_deref(), Some("http://example.test"));
            }
            result => panic!("unexpected response: {result:?}"),
        }
    }
}
