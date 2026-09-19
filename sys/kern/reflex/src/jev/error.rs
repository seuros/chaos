use std::time::Duration;

use chaos_client::TransportError;
use thiserror::Error;

use crate::error::bounded_body;

#[derive(Debug, Error)]
pub enum JevError {
    #[error("invalid question `{key}`: {reason}")]
    InvalidQuestion { key: String, reason: String },
    #[error("no questions supplied")]
    NoQuestions,
    #[error("unauthorized")]
    Unauthorized,
    #[error("request rejected: {0}")]
    Validation(String),
    #[error("rate limited")]
    RateLimited,
    #[error("overloaded")]
    Overloaded,
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("network: {0}")]
    Network(String),
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    #[error("decode: {0}")]
    Decode(String),
    #[error("answer `{0}` missing from response")]
    MissingAnswer(String),
}

impl JevError {
    pub(crate) fn from_transport(error: TransportError, timeout: Duration) -> Self {
        match error {
            TransportError::Http { status, body, .. } => match status.as_u16() {
                401 | 403 => Self::Unauthorized,
                400 | 422 => Self::Validation(bounded_body(body.as_deref().unwrap_or_default())),
                429 => Self::RateLimited,
                529 => Self::Overloaded,
                status => Self::Http {
                    status,
                    body: bounded_body(body.as_deref().unwrap_or_default()),
                },
            },
            TransportError::Timeout => Self::Timeout(timeout),
            TransportError::Build(message) => Self::Validation(message),
            error => Self::Network(error.to_string()),
        }
    }
}
