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
fn error_suite() {
    transport_error_classification();
    stream_error_classification();
}

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

fn stream_error_classification() {
    assert!(StreamError::Timeout.is_retryable());
    assert!(StreamError::Timeout.is_timeout());
    assert!(!StreamError::Stream("eof".into()).is_retryable());
    assert!(!StreamError::Stream("eof".into()).is_timeout());
}
