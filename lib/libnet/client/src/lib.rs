mod custom_ca;
mod default_client;
mod error;
mod request;
mod retry;
mod sse;
mod telemetry;
mod transport;

use std::sync::Once;

pub use crate::custom_ca::BuildCustomCaTransportError;
pub use crate::custom_ca::maybe_build_rustls_client_config_with_custom_ca;
pub use crate::default_client::CodexClientError;
pub use crate::default_client::CodexHttpClient;
pub use crate::default_client::CodexRequestBuilder;
pub use crate::default_client::CodexResponse;
pub use crate::error::StreamError;
pub use crate::error::TransportError;
pub use crate::request::Request;
pub use crate::request::RequestCompression;
pub use crate::request::Response;
pub use crate::retry::RetryOn;
pub use crate::retry::RetryPolicy;
pub use crate::retry::run_with_retry;
pub use crate::sse::sse_stream;
pub use crate::telemetry::RequestTelemetry;
pub use crate::transport::ByteStream;
pub use crate::transport::HttpTransport;
pub use crate::transport::RamaTransport;
pub use crate::transport::StreamResponse;

pub(crate) fn ensure_rustls_crypto_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rama::tls::rustls::dep::rustls::crypto::ring::default_provider().install_default();
    });
}
