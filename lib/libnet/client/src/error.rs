use chaos_abi::WireFormatError;
use rama::http::HeaderMap;
use rama::http::StatusCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("http {status}: {body:?}")]
    Http {
        status: StatusCode,
        url: Option<String>,
        headers: Option<HeaderMap>,
        body: Option<String>,
    },
    #[error("retry limit reached")]
    RetryLimit,
    #[error("timeout")]
    Timeout,
    #[error("network error: {0}")]
    Network(String),
    #[error("request build error: {0}")]
    Build(String),
}

impl WireFormatError for TransportError {
    fn is_retryable(&self) -> bool {
        match self {
            Self::Http { status, .. } => {
                status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error()
            }
            Self::Timeout | Self::Network(_) => true,
            Self::RetryLimit | Self::Build(_) => false,
        }
    }

    fn is_timeout(&self) -> bool {
        match self {
            Self::Timeout => true,
            Self::Http { status, .. } => status.as_u16() == 408,
            _ => false,
        }
    }
}

#[derive(Debug, Error)]
pub enum StreamError {
    #[error("stream failed: {0}")]
    Stream(String),
    #[error("timeout")]
    Timeout,
}

impl WireFormatError for StreamError {
    fn is_retryable(&self) -> bool {
        matches!(self, Self::Timeout)
    }

    fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout)
    }
}

#[cfg(test)]
mod tests;
