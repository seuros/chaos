use crate::config::OtelTlsConfig;
use crate::rama_otel_client::RamaOtelClient;

/// Build an HTTP client for OTLP HTTP exporters.
///
/// Returns a rama-based `RamaOtelClient` that implements the OpenTelemetry
/// `HttpClient` trait. OTEL exporters that run on non-tokio threads use
/// `block_in_place` or a dedicated thread to drive the async client. Client
/// construction itself is infallible; exporter and request failures remain
/// fallible.
pub(crate) fn build_http_client(_tls: &OtelTlsConfig, _timeout_var: &str) -> RamaOtelClient {
    // TODO: wire TLS config (custom CA, mTLS) into rama's rustls layer
    // when OTLP endpoints require it. For now, use the default client
    // which trusts system roots.
    RamaOtelClient::new()
}

pub(crate) fn current_tokio_runtime_is_multi_thread() -> bool {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread,
        Err(_) => false,
    }
}

pub(crate) fn build_async_http_client(
    _tls: Option<&OtelTlsConfig>,
    _timeout_var: &str,
) -> RamaOtelClient {
    // TODO: wire TLS config into rama's rustls layer for custom CA/mTLS.
    RamaOtelClient::new()
}

#[cfg(test)]
mod tests;
