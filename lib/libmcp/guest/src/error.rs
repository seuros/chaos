use std::time::Duration;

use chaos_abi::WireFormatError;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum GuestError {
    #[error("transport error: {0}")]
    Transport(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("server error {code}: {message}")]
    Server {
        code: i32,
        message: String,
        data: Option<Value>,
    },

    #[error("protocol version mismatch: sent {sent}, server {server}")]
    VersionMismatch { sent: String, server: String },

    #[error("unsupported protocol version: {0}")]
    UnsupportedProtocolVersion(String),

    #[error("request timed out after {0:?}")]
    Timeout(Duration),

    #[error("request cancelled")]
    Cancelled,

    #[error("session expired")]
    SessionExpired,

    #[error("disconnected")]
    Disconnected,

    #[error("invalid params: {0}")]
    InvalidParams(String),

    #[error("method not supported: {0}")]
    MethodNotSupported(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("http error: {0}")]
    Http(String),

    #[error("url parse error: {0}")]
    UrlParse(String),
}

impl GuestError {
    pub fn server_from_error(error: crate::protocol::JsonRpcError) -> Self {
        Self::Server {
            code: error.code,
            message: error.message,
            data: error.data,
        }
    }
}

impl WireFormatError for GuestError {
    fn is_retryable(&self) -> bool {
        match self {
            Self::Transport(_) | Self::Timeout(_) | Self::Disconnected | Self::Http(_) => true,
            Self::Json(_)
            | Self::Server { .. }
            | Self::VersionMismatch { .. }
            | Self::UnsupportedProtocolVersion(_)
            | Self::Cancelled
            | Self::SessionExpired
            | Self::InvalidParams(_)
            | Self::MethodNotSupported(_)
            | Self::Protocol(_)
            | Self::UrlParse(_) => false,
        }
    }

    fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn guest_error_classification_covers_main_variants() {
        let retryable: Vec<GuestError> = vec![
            GuestError::Transport(io::Error::other("conn dropped")),
            GuestError::Timeout(Duration::from_millis(50)),
            GuestError::Disconnected,
            GuestError::Http("502 bad gateway".into()),
        ];
        for err in &retryable {
            assert!(err.is_retryable(), "expected retryable for {err:?}");
        }
        // Only Timeout is a true timeout among the retryable group.
        assert!(retryable[1].is_timeout());
        assert!(!retryable[0].is_timeout());

        let terminal: Vec<GuestError> = vec![
            GuestError::Server {
                code: -32601,
                message: "method not found".into(),
                data: None,
            },
            GuestError::VersionMismatch {
                sent: "2024-11-05".into(),
                server: "2025-03-26".into(),
            },
            GuestError::Cancelled,
            GuestError::SessionExpired,
            GuestError::InvalidParams("missing field".into()),
            GuestError::Protocol("malformed".into()),
            GuestError::UrlParse("nope".into()),
        ];
        for err in &terminal {
            assert!(!err.is_retryable(), "expected non-retryable for {err:?}");
            assert!(!err.is_timeout(), "expected non-timeout for {err:?}");
            assert!(err.retry_after().is_none());
        }
    }
}
