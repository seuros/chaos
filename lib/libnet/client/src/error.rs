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
mod tests {
    use super::*;

    fn http(status: u16) -> TransportError {
        TransportError::Http {
            status: StatusCode::from_u16(status).expect("status"),
            url: None,
            headers: None,
            body: None,
        }
    }

    #[test]
    fn transport_error_classification() {
        let cases: &[(TransportError, bool, bool)] = &[
            (http(500), true, false),
            (http(429), true, false),
            (http(408), true, true),
            (http(400), false, false),
            (TransportError::Timeout, true, true),
            (TransportError::Network("conn reset".into()), true, false),
            (TransportError::RetryLimit, false, false),
            (TransportError::Build("bad uri".into()), false, false),
        ];
        for (err, retryable, timeout) in cases {
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
            assert!(err.retry_after().is_none());
        }
    }

    #[test]
    fn stream_error_classification() {
        assert!(StreamError::Timeout.is_retryable());
        assert!(StreamError::Timeout.is_timeout());
        assert!(!StreamError::Stream("eof".into()).is_retryable());
        assert!(!StreamError::Stream("eof".into()).is_timeout());
    }
}
