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
    assert_eq!(token("7").map(FencingToken::value), Some(7));
    assert!(token("0").is_none());
    assert!(token("-1").is_none());
    assert!(token("stored-token").is_none());
}

#[test]
fn acquisition_allows_absent_lease() {
    let grant = acquire_process(&ProcessId::new(), "writer", None, at(100), at(30))
        .expect("fresh authority grants");
    assert_eq!(grant.token, 1);
    assert_eq!(grant.last_issued, 1);
    assert_eq!(grant.expires_at, at(130));
}

#[test]
fn acquisition_allows_expired_and_at_now_leases_regardless_of_owner() {
    let process_id = ProcessId::new();
    for stored_owner in ["writer", "other"] {
        for expires_at in [at(99), at(100)] {
            let stored = row(stored_owner, Some(3), 3, expires_at);
            let grant = acquire_process(&process_id, "writer", Some(&stored), at(100), at(30))
                .expect("expired leases are replaceable");
            assert_eq!(grant.token, 4);
            assert_eq!(grant.last_issued, 4);
        }
    }
}

#[test]
fn acquisition_rotates_active_same_owner_token() {
    let stored = held(at(101));
    let grant = acquire_process(&ProcessId::new(), "writer", Some(&stored), at(100), at(30))
        .expect("same owner may rotate");
    assert_eq!(grant.token, 4);
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
    for corrupt in [
        row("writer", Some(5), 3, at(101)),
        row("writer", Some(0), 3, at(101)),
    ] {
        std::assert_matches!(
            acquire_process(&ProcessId::new(), "writer", Some(&corrupt), at(100), at(30)),
            Err(JournalError::LeaseHistory { .. })
        );
    }
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
    for expires_at in [at(99), at(100)] {
        std::assert_matches!(
            verify_process(&process_id, "writer", "3", Some(&held(expires_at)), at(100)),
            Err(JournalError::LeaseExpired { process_id: actual }) if actual == process_id
        );
        std::assert_matches!(
            renew_process(
                &process_id,
                "writer",
                "3",
                Some(&held(expires_at)),
                at(100),
                at(30)
            ),
            Err(JournalError::LeaseExpired { .. })
        );
    }
}

// The authority checks credentials before expiry: a writer whose lease was
// superseded is fenced rather than invited to reacquire.
#[test]
fn validation_rejects_credential_mismatch_even_when_expired() {
    let process_id = ProcessId::new();
    for expires_at in [at(99), at(101)] {
        for (owner_id, token) in [("other", "3"), ("writer", "2"), ("writer", "stored-token")] {
            std::assert_matches!(
                verify_process(&process_id, owner_id, token, Some(&held(expires_at)), at(100)),
                Err(JournalError::InvalidLease { process_id: actual }) if actual == process_id
            );
            std::assert_matches!(
                release_process(
                    &process_id,
                    owner_id,
                    token,
                    Some(&held(expires_at)),
                    at(100)
                ),
                Err(JournalError::InvalidLease { .. })
            );
        }
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

#[test]
fn renewal_keeps_token_and_high_water_and_never_shortens() {
    let process_id = ProcessId::new();
    let stored = row("writer", Some(3), 7, at(150));
    let shorter =
        renew_process(&process_id, "writer", "3", Some(&stored), at(100), at(10)).expect("renewal");
    assert_eq!(shorter.token, 3);
    assert_eq!(shorter.last_issued, 7);
    assert_eq!(shorter.expires_at, at(150));
    let longer =
        renew_process(&process_id, "writer", "3", Some(&stored), at(100), at(90)).expect("renewal");
    assert_eq!(longer.expires_at, at(190));
}
