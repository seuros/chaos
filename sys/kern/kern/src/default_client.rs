use chaos_ipc::product::CHAOS_VERSION;
use codex_client::ChaosHttpClient;
pub use codex_client::ChaosRequestBuilder;
use http::HeaderMap;
use http::HeaderValue;
use http::header::USER_AGENT;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::OnceLock;

/// Set this to add a suffix to the User-Agent string.
///
/// It is not ideal that we're using a global singleton for this.
/// This is primarily designed to differentiate MCP clients from each other.
/// Because there can only be one MCP server per process, it should be safe for this to be a global static.
/// However, future users of this should use this with caution as a result.
/// In addition, we want to be confident that this value is used for ALL clients and doing that requires a
/// lot of wiring and it's easy to miss code paths by doing so.
/// See upstream openai/codex#3388 for an example of what that would look like.
/// Finally, we want to make sure this is set for ALL mcp clients without needing to know a special env var
/// or having to set data that they already specified in the mcp initialize request somewhere else.
///
/// A space is automatically added between the suffix and the rest of the User-Agent string.
/// The full user agent string is returned from the mcp initialize response.
/// Parenthesis will be added by Chaos. This should only specify what goes inside of the parenthesis.
pub static USER_AGENT_SUFFIX: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));
pub const DEFAULT_ORIGINATOR: &str = "chaos_cli_rs";

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

fn build_originator(value: String) -> Originator {
    match HeaderValue::from_str(&value) {
        Ok(header_value) => Originator {
            value,
            header_value,
        },
        Err(e) => {
            tracing::error!("Invalid originator value, falling back to default: {e}");
            Originator {
                value: DEFAULT_ORIGINATOR.to_string(),
                header_value: HeaderValue::from_static(DEFAULT_ORIGINATOR),
            }
        }
    }
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
    ORIGINATOR.get_or_init(|| build_originator(DEFAULT_ORIGINATOR.to_string()))
}

pub fn get_chaos_user_agent() -> String {
    let os_info = os_info::get();
    let originator = originator();
    let prefix = format!(
        "{}/{} ({} {}; {}) {}",
        originator.value.as_str(),
        CHAOS_VERSION,
        os_info.os_type(),
        os_info.version(),
        os_info.architecture().unwrap_or("unknown"),
        crate::terminal::user_agent()
    );
    let suffix = USER_AGENT_SUFFIX
        .lock()
        .ok()
        .and_then(|guard| guard.clone());
    let suffix = suffix
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(String::new, |value| format!(" ({value})"));

    let candidate = format!("{prefix}{suffix}");
    sanitize_user_agent(candidate, &prefix)
}

/// Sanitize the user agent string.
///
/// Invalid characters are replaced with an underscore.
///
/// If the user agent fails to parse, it falls back to fallback and then to ORIGINATOR.
fn sanitize_user_agent(candidate: String, fallback: &str) -> String {
    if HeaderValue::from_str(candidate.as_str()).is_ok() {
        return candidate;
    }

    let sanitized: String = candidate
        .chars()
        .map(|ch| if matches!(ch, ' '..='~') { ch } else { '_' })
        .collect();
    if !sanitized.is_empty() && HeaderValue::from_str(sanitized.as_str()).is_ok() {
        tracing::warn!(
            "Sanitized Chaos user agent because provided suffix contained invalid header characters"
        );
        sanitized
    } else if HeaderValue::from_str(fallback).is_ok() {
        tracing::warn!(
            "Falling back to base Chaos user agent because provided suffix could not be sanitized"
        );
        fallback.to_string()
    } else {
        tracing::warn!(
            "Falling back to default Chaos originator because base user agent string is invalid"
        );
        originator().value.clone()
    }
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
    if let Ok(user_agent) = HeaderValue::from_str(&get_chaos_user_agent()) {
        headers.insert(USER_AGENT, user_agent);
    }
    headers
}

#[cfg(test)]
#[path = "default_client_tests.rs"]
mod tests;
