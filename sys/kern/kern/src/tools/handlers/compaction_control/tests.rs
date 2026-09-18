use super::*;

fn deferral() -> Deferral {
    Deferral {
        model: "model".to_string(),
        effective_context_window: 380_000,
        ceiling: 360_000,
    }
}

#[test]
fn stale_window_ids_are_rejected() {
    assert!(validate_window_id(Some("old"), "current").is_err());
    assert!(validate_window_id(Some("current"), "current").is_ok());
    assert!(validate_window_id(None, "current").is_ok());
}

#[test]
fn defer_once_requires_the_reflex() {
    assert!(
        validate_defer_once(
            &Control::Normal,
            false,
            false,
            320_000,
            350_000,
            &deferral()
        )
        .is_err()
    );
}

#[test]
fn repeated_defer_once_is_idempotent_even_at_the_ceiling() {
    let deferral = deferral();
    assert_eq!(
        validate_defer_once(
            &Control::Deferred(deferral.clone()),
            true,
            true,
            deferral.ceiling,
            350_000,
            &deferral,
        ),
        Ok(true)
    );
}

#[test]
fn second_distinct_deferral_is_refused() {
    assert!(
        validate_defer_once(&Control::Normal, true, true, 320_000, 350_000, &deferral()).is_err()
    );
}

#[test]
fn defer_once_cannot_replace_pending_compaction() {
    let request = CompactRequest {
        model: "model".to_string(),
        effective_context_window: 380_000,
    };
    assert!(
        validate_defer_once(
            &Control::CompactRequested(request),
            true,
            false,
            320_000,
            350_000,
            &deferral(),
        )
        .is_err()
    );
}

#[test]
fn defer_once_is_refused_at_the_ceiling() {
    let deferral = deferral();
    assert!(
        validate_defer_once(
            &Control::Normal,
            true,
            false,
            deferral.ceiling,
            350_000,
            &deferral,
        )
        .is_err()
    );
}
