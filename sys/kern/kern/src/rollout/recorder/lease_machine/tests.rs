use super::*;

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
