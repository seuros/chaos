use super::*;

#[test]
fn wire_format_classification_matches_variant_semantics() {
    let backoff = Some(Duration::from_millis(250));
    let cases: &[(AbiError, bool, bool, Option<Duration>)] = &[
        (AbiError::ServerOverloaded, true, false, None),
        (AbiError::ServiceUnavailable, true, false, None),
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
