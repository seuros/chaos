use super::*;

#[test]
fn persistence_status_notifies_all_listeners_only_on_changes() {
    let (tx, mut first) = watch::channel(PersistenceStatus::default());
    let mut second = tx.subscribe();
    update_status(&tx, |status| status.health = PersistenceHealth::Healthy);
    assert!(!first.has_changed().unwrap());
    assert!(!second.has_changed().unwrap());

    for health in [
        PersistenceHealth::Degraded,
        PersistenceHealth::Failing,
        PersistenceHealth::Failed,
        PersistenceHealth::Healthy,
    ] {
        update_status(&tx, |status| status.health = health);
        for rx in [&mut first, &mut second] {
            assert!(rx.has_changed().unwrap());
            assert_eq!(rx.borrow_and_update().health, health);
            assert!(!rx.has_changed().unwrap());
        }
        update_status(&tx, |status| status.health = health);
        assert!(!first.has_changed().unwrap());
    }
}

#[test]
fn persistence_status_retains_updates_without_listeners() {
    let tx = watch::channel(PersistenceStatus::default()).0;
    update_status(&tx, |status| status.health = PersistenceHealth::Failed);
    update_status(&tx, |status| {
        status.backend = RuntimeStorageBackend::Postgres
    });
    let mut rx = tx.subscribe();
    assert_eq!(
        *rx.borrow(),
        PersistenceStatus {
            health: PersistenceHealth::Failed,
            backend: RuntimeStorageBackend::Postgres,
        }
    );
    update_status(&tx, |status| {
        status.backend = RuntimeStorageBackend::Postgres
    });
    assert!(!rx.has_changed().unwrap());
    update_status(&tx, |status| status.backend = RuntimeStorageBackend::Sqlite);
    assert!(rx.has_changed().unwrap());
    assert_eq!(
        rx.borrow_and_update().backend,
        RuntimeStorageBackend::Sqlite
    );
    assert_eq!(rx.borrow().health, PersistenceHealth::Failed);
}
