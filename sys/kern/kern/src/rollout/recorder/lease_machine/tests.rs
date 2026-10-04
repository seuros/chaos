use super::*;

#[test]
fn expiry_requires_reconciliation_and_fencing_is_terminal() {
    let mut lease = Lease::default();
    lease.apply(WriterLeaseEvent::Expire);
    assert!(lease.needs_reacquire());
    lease.apply(WriterLeaseEvent::Refresh);
    assert!(lease.needs_reacquire());
    assert!(
        lease
            .machine
            .handle(WriterLeaseEvent::Confirm(Instant::now()))
            .is_err()
    );
    lease.apply(WriterLeaseEvent::Reacquire);
    assert!(!lease.confirmed());
    lease.apply(WriterLeaseEvent::Confirm(Instant::now()));
    assert!(lease.confirmed());
    lease.apply(WriterLeaseEvent::Fence);
    assert!(lease.machine.handle(WriterLeaseEvent::Refresh).is_err());
    assert!(lease.machine.handle(WriterLeaseEvent::Reacquire).is_err());
    assert!(
        lease
            .machine
            .handle(WriterLeaseEvent::Confirm(Instant::now()))
            .is_err()
    );
    assert!(lease.fenced());
}
