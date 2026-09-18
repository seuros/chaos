use std::time::Duration;

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
    pub fn clone_for_fanout(&self) -> Self {
        match self {
            Self::Disconnected => Self::Disconnected,
            Self::Cancelled => Self::Cancelled,
            Self::SessionExpired => Self::SessionExpired,
            Self::Timeout(duration) => Self::Timeout(*duration),
            Self::InvalidParams(message) => Self::InvalidParams(message.clone()),
            Self::MethodNotSupported(method) => Self::MethodNotSupported(method.clone()),
            Self::Protocol(message) => Self::Protocol(message.clone()),
            Self::Http(message) => Self::Http(message.clone()),
            Self::UrlParse(message) => Self::UrlParse(message.clone()),
            Self::UnsupportedProtocolVersion(version) => {
                Self::UnsupportedProtocolVersion(version.clone())
            }
            Self::VersionMismatch { sent, server } => Self::VersionMismatch {
                sent: sent.clone(),
                server: server.clone(),
            },
            Self::Server {
                code,
                message,
                data,
            } => Self::Server {
                code: *code,
                message: message.clone(),
                data: data.clone(),
            },
            Self::Transport(io) => Self::Http(io.to_string()),
            Self::Json(json) => Self::Protocol(json.to_string()),
        }
    }

    pub fn server_from_error(error: crate::protocol::JsonRpcError) -> Self {
        if error.code == crate::protocol::UNSUPPORTED_PROTOCOL_VERSION {
            let requested = error
                .data
                .as_ref()
                .and_then(|data| data.get("requested"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            return Self::UnsupportedProtocolVersion(requested);
        }

        Self::Server {
            code: error.code,
            message: error.message,
            data: error.data,
        }
    }
}

impl GuestError {
    /// Whether the operation may succeed if attempted again.
    pub fn is_retryable(&self) -> bool {
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

    /// Whether the failure was caused by a request deadline.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout(_))
    }

    /// Suggested retry delay, if supplied by the error.
    ///
    /// Guest errors currently carry no retry-delay information.
    pub fn retry_after(&self) -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests;
