use super::*;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).expect("valid test timestamp")
}

#[test]
fn acquisition_allows_absent_lease() {
    assert!(ensure_acquirable(&ProcessId::new(), &"writer".into(), None, at(100)).is_ok());
}

#[test]
fn acquisition_allows_expired_and_at_now_leases_regardless_of_owner() {
    let process_id = ProcessId::new();
    let owner_id = "writer".to_string();
    for stored_owner in ["writer", "other"] {
        for expires_at in [at(99), at(100)] {
            assert!(
                ensure_acquirable(
                    &process_id,
                    &owner_id,
                    Some((stored_owner.into(), expires_at)),
                    at(100),
                )
                .is_ok()
            );
        }
    }
}

#[test]
fn acquisition_allows_active_same_owner() {
    let owner_id = "writer".to_string();
    assert!(
        ensure_acquirable(
            &ProcessId::new(),
            &owner_id,
            Some((owner_id.clone(), at(101))),
            at(100),
        )
        .is_ok()
    );
}

#[test]
fn acquisition_rejects_active_foreign_owner_with_original_details() {
    let process_id = ProcessId::new();
    // Do not round `now` to database seconds when deciding whether a lease is active.
    let now = Timestamp::from_millisecond(100_999).expect("valid test timestamp");
    match ensure_acquirable(
        &process_id,
        &"writer".into(),
        Some(("other".into(), at(101))),
        now,
    ) {
        Err(JournalError::LeaseConflict {
            process_id: actual_process,
            current_owner_id,
            expires_at,
        }) => {
            assert_eq!(actual_process, process_id);
            assert_eq!(current_owner_id, "other");
            assert_eq!(expires_at, at(101));
        }
        other => panic!("expected lease conflict, got {other:?}"),
    }
}

fn stored_lease(process_id: ProcessId, expires_at: Timestamp) -> Lease {
    Lease {
        process_id,
        owner_id: "writer".into(),
        lease_token: "stored-token".into(),
        expires_at,
    }
}

#[test]
fn validation_rejects_absent_lease() {
    let process_id = ProcessId::new();
    std::assert_matches!(
        validate_lease(None, &process_id, &"writer".into(), "stored-token", at(100)),
        Err(JournalError::InvalidLease { process_id: actual }) if actual == process_id
    );
}

#[test]
fn validation_rejects_expired_and_at_now_before_checking_credentials() {
    let process_id = ProcessId::new();
    for expires_at in [at(99), at(100)] {
        for (owner_id, token) in [("writer", "stored-token"), ("other", "wrong-token")] {
            std::assert_matches!(
                validate_lease(
                    Some(stored_lease(process_id, expires_at)),
                    &process_id,
                    &owner_id.into(),
                    token,
                    at(100),
                ),
                Err(JournalError::LeaseExpired { process_id: actual }) if actual == process_id
            );
        }
    }
}

#[test]
fn validation_rejects_active_owner_or_token_mismatch() {
    let process_id = ProcessId::new();
    for (owner_id, token) in [
        ("other", "stored-token"),
        ("writer", "wrong-token"),
        ("other", "wrong-token"),
    ] {
        std::assert_matches!(
            validate_lease(
                Some(stored_lease(process_id, at(101))),
                &process_id,
                &owner_id.into(),
                token,
                at(100),
            ),
            Err(JournalError::InvalidLease { process_id: actual }) if actual == process_id
        );
    }
}

#[test]
fn validation_returns_active_matching_lease_unchanged() {
    let process_id = ProcessId::new();
    let lease = stored_lease(process_id, at(101));
    let validated = validate_lease(
        Some(lease.clone()),
        &process_id,
        &lease.owner_id,
        &lease.lease_token,
        at(100),
    )
    .expect("matching active lease");
    assert_eq!(validated, lease);
}
