use super::*;

#[test]
fn expiry_requires_reconciliation_and_fencing_is_terminal() {
    let mut lease = Lease::default();
    lease.expire().expect("an unfenced lease expires");
    assert!(lease.needs_reacquire());
    lease
        .refresh(Instant::now())
        .expect("an expired lease may refresh");
    assert!(lease.needs_reacquire());
    assert!(!lease.confirm("1", Instant::now()));
    assert!(lease.needs_reacquire());
    assert!(lease.reacquire("2", Instant::now()));
    assert!(lease.confirmed());
    lease.fence();
    assert_eq!(lease.refresh(Instant::now()), Err(Refusal::Fenced));
    assert_eq!(lease.expire(), Err(Refusal::Fenced));
    assert!(!lease.reacquire("3", Instant::now()));
    assert!(!lease.confirm("3", Instant::now()));
    assert!(lease.fenced());
    assert!(!lease.renewal_due(Instant::now()));
}

#[test]
fn numeric_tokens_order_replies() {
    let start = Instant::now();
    let mut lease = Lease::new("7", start).expect("7 is a fencing token");
    assert_eq!(lease.holder.token().raw(), 7);
    lease
        .refresh(start + JOURNAL_LEASE_REFRESH_INTERVAL)
        .expect("refresh");
    assert!(!lease.confirm("6", start + JOURNAL_LEASE_REFRESH_INTERVAL));
    assert!(!lease.confirmed());
    assert!(lease.confirm("7", start + JOURNAL_LEASE_REFRESH_INTERVAL));
    assert!(lease.confirmed());
    lease.expire().expect("expiry");
    assert!(!lease.reacquire("7", start + JOURNAL_LEASE_TTL));
    assert!(lease.reacquire("8", start + JOURNAL_LEASE_TTL));
}

#[test]
fn the_authority_owns_tokens_and_opaque_ones_are_refused() {
    let start = Instant::now();
    assert!(Lease::new("4f2c1d9e-opaque", start).is_none());
    assert!(Lease::new("0", start).is_none());

    let mut lease = Lease::new("3", start).expect("3 is a fencing token");
    lease
        .refresh(start + JOURNAL_LEASE_REFRESH_INTERVAL)
        .expect("refresh");
    assert!(!lease.confirm("4f2c1d9e-opaque", start + JOURNAL_LEASE_REFRESH_INTERVAL));
    assert!(!lease.confirmed());
    assert_eq!(lease.holder.token().raw(), 3);
}

#[test]
fn stale_confirm_after_fence_is_ignored() {
    let start = Instant::now();
    let mut lease = Lease::new("1", start).expect("1 is a fencing token");
    lease
        .refresh(start + JOURNAL_LEASE_REFRESH_INTERVAL)
        .expect("refresh");
    lease.fence();
    assert!(!lease.confirm("1", start + JOURNAL_LEASE_REFRESH_INTERVAL));
    assert!(!lease.confirm("2", start + JOURNAL_LEASE_REFRESH_INTERVAL));
    assert!(lease.fenced());
    assert!(!lease.confirmed());
    assert_eq!(lease.warning_deadline(), None);
}
