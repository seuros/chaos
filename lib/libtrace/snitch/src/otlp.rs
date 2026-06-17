use crate::config::OtelTlsConfig;
use crate::rama_otel_client::RamaOtelClient;
use std::error::Error;

/// Build an HTTP client for OTLP HTTP exporters.
///
/// Returns a rama-based `RamaOtelClient` that implements the OpenTelemetry
/// `HttpClient` trait. OTEL exporters that run on non-tokio threads use
/// `block_in_place` or a dedicated thread to drive the async client.
pub(crate) fn build_http_client(
    _tls: &OtelTlsConfig,
    _timeout_var: &str,
) -> Result<RamaOtelClient, Box<dyn Error>> {
    // TODO: wire TLS config (custom CA, mTLS) into rama's rustls layer
    // when OTLP endpoints require it. For now, use the default client
    // which trusts system roots.
    Ok(RamaOtelClient::new())
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
) -> Result<RamaOtelClient, Box<dyn Error>> {
    // TODO: wire TLS config into rama's rustls layer for custom CA/mTLS.
    Ok(RamaOtelClient::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tokio::runtime::Builder;

    #[test]
    fn current_tokio_runtime_is_multi_thread_detects_runtime_flavor() {
        assert!(!current_tokio_runtime_is_multi_thread());

        let current_thread_runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");
        assert_eq!(
            current_thread_runtime.block_on(async { current_tokio_runtime_is_multi_thread() }),
            false
        );

        let multi_thread_runtime = Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("multi-thread runtime");
        assert_eq!(
            multi_thread_runtime.block_on(async { current_tokio_runtime_is_multi_thread() }),
            true
        );
    }
}
