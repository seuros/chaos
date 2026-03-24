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
    pub fn server_from_error(error: crate::protocol::JsonRpcError) -> Self {
        Self::Server {
            code: error.code,
            message: error.message,
            data: error.data,
        }
    }
}
