use chaos_client::ChaosHttpClient;
pub use chaos_client::ChaosRequestBuilder;
use chaos_ipc::product::CHAOS_VERSION;
use rama::http::HeaderMap;
use rama::http::HeaderValue;
use rama::http::header::USER_AGENT;
use std::sync::OnceLock;

/// Default identity for the originator header and User-Agent prefix.
pub const DEFAULT_ORIGINATOR: &str = "free_chaos";

#[derive(Debug, Clone)]
pub struct Originator {
    pub value: String,
    pub header_value: HeaderValue,
}
static ORIGINATOR: OnceLock<Originator> = OnceLock::new();

#[derive(Debug)]
pub enum SetOriginatorError {
    InvalidHeaderValue,
    AlreadyInitialized,
}

pub fn set_default_originator(value: String) -> Result<(), SetOriginatorError> {
    let Ok(header_value) = HeaderValue::from_str(&value) else {
        return Err(SetOriginatorError::InvalidHeaderValue);
    };
    ORIGINATOR
        .set(Originator {
            value,
            header_value,
        })
        .map_err(|_| SetOriginatorError::AlreadyInitialized)
}

pub fn originator() -> &'static Originator {
    ORIGINATOR.get_or_init(|| Originator {
        value: DEFAULT_ORIGINATOR.to_string(),
        header_value: HeaderValue::from_static(DEFAULT_ORIGINATOR),
    })
}

/// Returns `<originator>/<version>` without OS, architecture, or terminal details.
pub fn get_chaos_user_agent() -> String {
    chaos_user_agent().0
}

fn chaos_user_agent() -> (String, HeaderValue) {
    let originator = originator();
    let candidate = format!("{}/{}", originator.value, CHAOS_VERSION);
    sanitize_user_agent(candidate)
}

/// Sanitize the user agent string.
///
/// Invalid characters are replaced with an underscore.
///
/// Returns both the display string and the validated HTTP header so callers
/// do not need to validate the sanitized value again.
#[allow(
    clippy::expect_used,
    reason = "sanitization produces only printable ASCII"
)]
fn sanitize_user_agent(candidate: String) -> (String, HeaderValue) {
    if let Ok(header) = HeaderValue::from_str(&candidate) {
        return (candidate, header);
    }

    let sanitized: String = candidate
        .chars()
        .map(|ch| if matches!(ch, ' '..='~') { ch } else { '_' })
        .collect();
    let header =
        HeaderValue::from_str(&sanitized).expect("printable ASCII is a valid HTTP header value");
    tracing::warn!("Sanitized Chaos user agent because it contained invalid header characters");
    (sanitized, header)
}

/// Create an HTTP client with default `originator` and `User-Agent` headers set.
pub fn create_client() -> ChaosHttpClient {
    // Custom CA support uses SSL_CERT_FILE, handled by the rustls/system root store.
    ChaosHttpClient::default_client().with_default_headers(default_headers())
}

/// Builds the default rama HTTP client used for ordinary ChaOS HTTP traffic.
///
/// This is the infallible entry point for call sites that previously used
/// `build_http_client()`. Returns a ChaosHttpClient backed by rama.
pub fn build_http_client() -> ChaosHttpClient {
    create_client()
}

pub fn default_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("originator", originator().header_value.clone());
    headers.insert(USER_AGENT, chaos_user_agent().1);
    headers
}

#[cfg(test)]
#[path = "default_client_tests.rs"]
mod tests;
