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

    /// The request was rejected as invalid.
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },

    /// A streaming error occurred.
    #[error("stream error: {0}")]
    Stream(String),

    /// An HTTP-level transport error.
    #[error("transport error: HTTP {status} — {message}")]
    Transport { status: u16, message: String },

    /// A transient error that may succeed on retry.
    #[error("retryable: {message}")]
    Retryable {
        message: String,
        delay: Option<Duration>,
    },
}

impl WireFormatError for AbiError {
    fn is_retryable(&self) -> bool {
        match self {
            Self::ServerOverloaded | Self::Retryable { .. } => true,
            Self::Transport { status, .. } => {
                *status == 408 || *status == 429 || (*status >= 500 && *status < 600)
            }
            Self::ContextWindowExceeded
            | Self::QuotaExceeded
            | Self::UsageNotIncluded
            | Self::InvalidRequest { .. }
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
mod tests {
    use super::*;

    #[test]
    fn wire_format_classification_matches_variant_semantics() {
        let backoff = Some(Duration::from_millis(250));
        let cases: &[(AbiError, bool, bool, Option<Duration>)] = &[
            (AbiError::ServerOverloaded, true, false, None),
            (
                AbiError::Retryable {
                    message: "slow down".into(),
                    delay: backoff,
                },
                true,
                false,
                backoff,
            ),
            (
                AbiError::Transport {
                    status: 503,
                    message: "upstream".into(),
                },
                true,
                false,
                None,
            ),
            (
                AbiError::Transport {
                    status: 408,
                    message: "deadline".into(),
                },
                true,
                true,
                None,
            ),
            (
                AbiError::Transport {
                    status: 400,
                    message: "bad json".into(),
                },
                false,
                false,
                None,
            ),
            (AbiError::ContextWindowExceeded, false, false, None),
            (AbiError::QuotaExceeded, false, false, None),
            (
                AbiError::InvalidRequest {
                    message: "no model".into(),
                },
                false,
                false,
                None,
            ),
            (AbiError::Stream("eof".into()), false, false, None),
        ];

        for (err, retryable, timeout, retry_after) in cases {
            assert_eq!(
                err.is_retryable(),
                *retryable,
                "is_retryable mismatch for {err:?}"
            );
            assert_eq!(
                err.is_timeout(),
                *timeout,
                "is_timeout mismatch for {err:?}"
            );
            assert_eq!(
                err.retry_after(),
                *retry_after,
                "retry_after mismatch for {err:?}"
            );
        }
    }
}
