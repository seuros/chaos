//! Provider-neutral error types returned by model adapters.

use std::time::Duration;

use crate::wire_error::WireFormatError;

/// Errors that [`ModelAdapter`](crate::ModelAdapter) implementations return.
#[derive(Debug, thiserror::Error)]
pub enum AbiError {
    /// The conversation exceeds the model's context window.
    #[error("context window exceeded")]
    ContextWindowExceeded,

    /// Billing quota exhausted.
    #[error("quota exceeded")]
    QuotaExceeded,

    /// Usage information was not included in the response.
    #[error("usage not included")]
    UsageNotIncluded,

    /// The provider's servers are overloaded.
    #[error("server overloaded")]
    ServerOverloaded,

    /// The provider is temporarily unavailable.
    #[error("service unavailable")]
    ServiceUnavailable,

    /// The request was rejected as invalid.
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },

    /// The provider could not decrypt reasoning content echoed back in the input.
    #[error("invalid encrypted content: {message}")]
    InvalidEncryptedContent { message: String },

    /// A streaming error occurred.
    #[error("stream error: {0}")]
    Stream(String),

    /// An HTTP-level transport error.
    #[error("transport error: HTTP {status} — {message}")]
    Transport { status: u16, message: String },

    /// The model declined to answer. Retrying the same request will not help.
    #[error("model refused{}", refusal_detail(category.as_deref(), explanation.as_deref()))]
    Refusal {
        category: Option<String>,
        explanation: Option<String>,
    },

    /// A transient error that may succeed on retry.
    #[error("retryable: {message}")]
    Retryable {
        message: String,
        delay: Option<Duration>,
    },
}

fn refusal_detail(category: Option<&str>, explanation: Option<&str>) -> String {
    let category = category.map(|c| format!(" ({c})")).unwrap_or_default();
    let explanation = explanation.map(|e| format!(": {e}")).unwrap_or_default();
    format!("{category}{explanation}")
}

impl WireFormatError for AbiError {
    fn is_retryable(&self) -> bool {
        match self {
            Self::ServerOverloaded | Self::ServiceUnavailable | Self::Retryable { .. } => true,
            Self::Transport { status, .. } => {
                *status == 408 || *status == 429 || (*status >= 500 && *status < 600)
            }
            Self::ContextWindowExceeded
            | Self::QuotaExceeded
            | Self::UsageNotIncluded
            | Self::InvalidRequest { .. }
            | Self::InvalidEncryptedContent { .. }
            | Self::Refusal { .. }
            | Self::Stream(_) => false,
        }
    }

    fn is_timeout(&self) -> bool {
        matches!(self, Self::Transport { status: 408, .. })
    }

    fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Retryable { delay, .. } => *delay,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
