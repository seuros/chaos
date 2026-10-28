use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_factories_run_once_and_promote_hits() {
    let cache = BlockingLruCache::try_with_capacity(2).unwrap();
    assert_eq!(cache.get_or_insert_with("first", || 1), 1);
    assert_eq!(cache.get_or_insert_with("second", || 2), 2);
    assert_eq!(
        cache.get_or_insert_with("first", || panic!("cached factory ran")),
        1
    );
    assert_eq!(cache.get_or_insert_with("third", || 3), 3);
    assert_eq!(cache.get(&"first"), Some(1));
    assert_eq!(cache.get(&"second"), None);
    assert_eq!(cache.get(&"third"), Some(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_factories_do_not_insert_or_evict() {
    let cache = BlockingLruCache::new(NonZeroUsize::MIN);
    cache.insert("retained", 1);
    assert_eq!(
        cache.get_or_try_insert_with("retry", || Err("failed")),
        Err("failed")
    );
    assert_eq!(cache.get(&"retained"), Some(1));
    assert_eq!(cache.get(&"retry"), None);
    assert_eq!(
        cache.get_or_try_insert_with("retry", || Ok::<_, &str>(2)),
        Ok(2)
    );
    assert_eq!(
        cache.get_or_try_insert_with("retry", || Err("must not run")),
        Ok(2)
    );
    assert_eq!(cache.get(&"retained"), None);
}

#[test]
fn factories_are_passthrough_without_a_runtime() {
    let cache = BlockingLruCache::new(NonZeroUsize::MIN);
    assert_eq!(cache.get_or_insert_with("key", || 1), 1);
    assert_eq!(cache.get_or_insert_with("key", || 2), 2);
    assert_eq!(
        cache.get_or_try_insert_with("key", || Err::<i32, _>("failed")),
        Err("failed")
    );
    assert_eq!(cache.get(&"key"), None);
    assert!(BlockingLruCache::<(), ()>::try_with_capacity(0).is_none());
}
