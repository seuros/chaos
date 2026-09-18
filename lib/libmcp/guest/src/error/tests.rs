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
