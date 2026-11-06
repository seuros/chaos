use super::*;

#[test]
fn expiry_requires_reconciliation_and_fencing_is_terminal() {
    let mut lease = Lease::default();
    lease.expire();
    assert!(lease.needs_reacquire());
    lease.refresh(Instant::now());
    assert!(lease.needs_reacquire());
    assert!(!lease.confirm("renewed", Instant::now()));
    assert!(lease.needs_reacquire());
    assert!(lease.reacquire("fresh", Instant::now()));
    assert!(lease.confirmed());
    lease.fence();
    lease.refresh(Instant::now());
    assert!(!lease.reacquire("later", Instant::now()));
    assert!(!lease.confirm("later", Instant::now()));
    assert!(lease.fenced());
    assert!(!lease.renewal_due(Instant::now()));
}

#[test]
fn numeric_tokens_order_replies_and_opaque_ones_advance_locally() {
    let start = Instant::now();
    let mut lease = Lease::new("7", start);
    assert_eq!(lease.holder.token().value(), 7);
    lease.refresh(start + JOURNAL_LEASE_REFRESH_INTERVAL);
    assert!(!lease.confirm("6", start + JOURNAL_LEASE_REFRESH_INTERVAL));
    assert!(!lease.confirmed());
    assert!(lease.confirm("7", start + JOURNAL_LEASE_REFRESH_INTERVAL));
    assert!(lease.confirmed());
    lease.expire();
    assert!(!lease.reacquire("7", start + JOURNAL_LEASE_TTL));
    assert!(lease.reacquire("8", start + JOURNAL_LEASE_TTL));

    let mut opaque = Lease::new("4f2c1d9e-opaque", start);
    assert_eq!(opaque.holder.token().value(), 1);
    opaque.refresh(start + JOURNAL_LEASE_REFRESH_INTERVAL);
    assert!(opaque.confirm("4f2c1d9e-opaque", start + JOURNAL_LEASE_REFRESH_INTERVAL));
    assert_eq!(opaque.holder.token().value(), 2);
}

#[test]
fn stale_confirm_after_fence_is_ignored() {
    let start = Instant::now();
    let mut lease = Lease::new("1", start);
    lease.refresh(start + JOURNAL_LEASE_REFRESH_INTERVAL);
    lease.fence();
    assert!(!lease.confirm("1", start + JOURNAL_LEASE_REFRESH_INTERVAL));
    assert!(!lease.confirm("2", start + JOURNAL_LEASE_REFRESH_INTERVAL));
    assert!(lease.fenced());
    assert!(!lease.confirmed());
    assert_eq!(lease.warning_deadline(), None);
}
