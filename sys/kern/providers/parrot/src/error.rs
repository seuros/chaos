use crate::rate_limits::RateLimitError;
use chaos_client::TransportError;
use rama::http::StatusCode;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error(transparent)]
    Transport(TransportError),
    #[error("api error {status}: {message}")]
    Api { status: StatusCode, message: String },
    #[error("stream error: {0}")]
    Stream(String),
    #[error("context window exceeded")]
    ContextWindowExceeded,
    #[error("quota exceeded")]
    QuotaExceeded,
    #[error("usage not included")]
    UsageNotIncluded,
    #[error("retryable error: {message}")]
    Retryable {
        message: String,
        delay: Option<Duration>,
    },
    #[error("rate limit: {0}")]
    RateLimit(String),
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },
    #[error("invalid encrypted content: {message}")]
    InvalidEncryptedContent { message: String },
    #[error("server overloaded")]
    ServerOverloaded,
    #[error("service unavailable")]
    ServiceUnavailable,
}

impl From<TransportError> for ApiError {
    fn from(err: TransportError) -> Self {
        if let Some(message) = invalid_encrypted_content_message(&err) {
            Self::InvalidEncryptedContent { message }
        } else if is_chatgpt_backend_outage(&err) {
            Self::ServiceUnavailable
        } else {
            Self::Transport(err)
        }
    }
}

fn invalid_encrypted_content_message(err: &TransportError) -> Option<String> {
    let TransportError::Http {
        body: Some(body), ..
    } = err
    else {
        return None;
    };
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    if value["error"]["code"].as_str() != Some("invalid_encrypted_content") {
        return None;
    }
    Some(
        value["error"]["message"]
            .as_str()
            .unwrap_or("provider rejected encrypted reasoning content")
            .to_owned(),
    )
}

fn is_chatgpt_backend_outage(err: &TransportError) -> bool {
    // Cloudflare can return a blank 404 for valid ChatGPT backend routes during
    // an outage. Provider 404s with a body still carry meaningful API details.
    let TransportError::Http {
        status, url, body, ..
    } = err
    else {
        return false;
    };
    if *status != StatusCode::NOT_FOUND
        || body.as_deref().is_some_and(|body| !body.trim().is_empty())
    {
        return false;
    }

    let Some(url) = url.as_deref() else {
        return false;
    };
    url.strip_prefix(chaos_services::openai::CHATGPT_BACKEND_BASE)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

impl From<RateLimitError> for ApiError {
    fn from(err: RateLimitError) -> Self {
        Self::RateLimit(err.to_string())
    }
}
