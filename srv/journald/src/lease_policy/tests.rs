use super::*;

fn at(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

fn row(owner_id: &str, token: Option<u64>, last_issued: u64, expires_at: Duration) -> LeaseRow {
    LeaseRow {
        owner_id: owner_id.into(),
        token,
        last_issued,
        expires_at,
    }
}

fn held(expires_at: Duration) -> LeaseRow {
    row("writer", Some(3), 3, expires_at)
}

#[test]
fn owner_digest_is_stable_and_distinguishes_owners() {
    assert_eq!(owner("writer"), owner("writer"));
    assert_ne!(owner("writer"), owner("other"));
    assert_eq!(owner("").value(), 0xcbf2_9ce4_8422_2325);
}

#[test]
fn tokens_parse_only_positive_decimals() {
    assert_eq!(token("7").map(FencingToken::raw), Some(7));
    assert!(token("0").is_none());
    assert!(token("-1").is_none());
    assert!(token("stored-token").is_none());
}

#[test]
fn acquisition_continues_from_high_water_after_release() {
    let released = row("writer", None, 9, Duration::ZERO);
    let grant = acquire_process(&ProcessId::new(), "other", Some(&released), at(100), at(30))
        .expect("released resources are free");
    assert_eq!(grant.token, 10);
}

#[test]
fn acquisition_rejects_active_foreign_owner_with_original_details() {
    let process_id = ProcessId::new();
    // Do not round `now` to database seconds when deciding whether a lease is active.
    let now = Duration::from_millis(100_999);
    let stored = row("other", Some(3), 3, at(101));
    match acquire_process(&process_id, "writer", Some(&stored), now, at(30)) {
        Err(JournalError::LeaseConflict {
            process_id: actual_process,
            current_owner_id,
            expires_at,
        }) => {
            assert_eq!(actual_process, process_id);
            assert_eq!(current_owner_id, "other");
            assert_eq!(expires_at.as_second(), 101);
        }
        other => panic!("expected lease conflict, got {other:?}"),
    }
}

#[test]
fn restore_rejects_tokens_the_history_never_issued() {
    let corrupt = row("writer", Some(5), 3, at(101));
    std::assert_matches!(
        acquire_process(&ProcessId::new(), "writer", Some(&corrupt), at(100), at(30)),
        Err(JournalError::LeaseHistory { .. })
    );
}

#[test]
fn validation_rejects_absent_and_released_leases() {
    let process_id = ProcessId::new();
    let released = row("writer", None, 3, Duration::ZERO);
    for stored in [None, Some(&released)] {
        std::assert_matches!(
            verify_process(&process_id, "writer", "3", stored, at(100)),
            Err(JournalError::InvalidLease { process_id: actual }) if actual == process_id
        );
    }
}

#[test]
fn validation_reports_expiry_to_the_lease_holder() {
    let process_id = ProcessId::new();
    let expired = held(at(99));
    std::assert_matches!(
        verify_process(&process_id, "writer", "3", Some(&expired), at(100)),
        Err(JournalError::LeaseExpired { process_id: actual }) if actual == process_id
    );
    std::assert_matches!(
        renew_process(&process_id, "writer", "3", Some(&expired), at(100), at(30)),
        Err(JournalError::LeaseExpired { .. })
    );
}

#[test]
fn validation_rejects_credential_mismatch() {
    let process_id = ProcessId::new();
    let live = held(at(101));
    for (owner_id, token) in [("other", "3"), ("writer", "2"), ("writer", "stored-token")] {
        std::assert_matches!(
            verify_process(&process_id, owner_id, token, Some(&live), at(100)),
            Err(JournalError::InvalidLease { process_id: actual }) if actual == process_id
        );
        std::assert_matches!(
            release_process(&process_id, owner_id, token, Some(&live), at(100)),
            Err(JournalError::InvalidLease { .. })
        );
    }
}

#[test]
fn validation_accepts_active_matching_lease() {
    let process_id = ProcessId::new();
    verify_process(&process_id, "writer", "3", Some(&held(at(101))), at(100))
        .expect("matching active lease");
    release_process(&process_id, "writer", "3", Some(&held(at(101))), at(100))
        .expect("holder may release");
}
