use super::*;

#[test]
fn stale_window_ids_are_rejected() {
    assert!(validate_window_id(Some("old"), "current").is_err());
    assert!(validate_window_id(Some("current"), "current").is_ok());
    assert!(validate_window_id(None, "current").is_ok());
}

#[test]
fn deferral_requires_reflex_and_capacity_but_retries_are_idempotent() {
    let deferral = Deferral {
        model: "model".to_string(),
        effective_context_window: 380_000,
        ceiling: 360_000,
    };
    for (label, control, reflex, used, active, expected) in [
        (
            "before reflex",
            Control::Normal,
            false,
            false,
            320_000,
            None,
        ),
        (
            "fresh request",
            Control::Normal,
            true,
            false,
            320_000,
            Some(false),
        ),
        ("already used", Control::Normal, true, true, 320_000, None),
        (
            "at ceiling",
            Control::Normal,
            true,
            false,
            deferral.ceiling,
            None,
        ),
        (
            "same retry at ceiling",
            Control::Deferred(deferral.clone()),
            true,
            true,
            deferral.ceiling,
            Some(true),
        ),
        (
            "compaction pending",
            Control::CompactRequested(CompactRequest {
                model: "model".to_string(),
                effective_context_window: 380_000,
            }),
            true,
            false,
            320_000,
            None,
        ),
    ] {
        assert_eq!(
            validate_defer_once(&control, reflex, used, active, 350_000, &deferral).ok(),
            expected,
            "{label}"
        );
    }
}
