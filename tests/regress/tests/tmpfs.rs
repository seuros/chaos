//! Public-API tests for `chaos-tmpfs` — a cache that fails closed when
//! the runtime isn't looking.
//!
//! `BlockingLruCache` is deliberately inert outside a Tokio runtime:
//! every mutating call is a quiet no-op so callers can share the same
//! cache across sync and async contexts without spinning a runtime just
//! to serve a miss. The tests pin both halves of that contract: LRU
//! eviction under a live runtime, and the full disabled surface without
//! one.

use std::num::NonZeroUsize;

use chaos_tmpfs::BlockingLruCache;

#[tokio::test(flavor = "multi_thread")]
async fn lru_cache_evicts_least_recently_used_entry_under_runtime() {
    let cache = BlockingLruCache::new(NonZeroUsize::new(2).expect("capacity"));

    // Empty-cache miss is `None`, not a panic.
    assert!(cache.get(&"first").is_none());
    cache.insert("first", 1);
    assert_eq!(cache.get(&"first"), Some(1));

    // Fill to capacity, then touch "a" so it's the most-recent. Inserting
    // "c" must evict "b", not "a".
    cache.insert("a", 1);
    cache.insert("b", 2);
    assert_eq!(cache.get(&"a"), Some(1));

    cache.insert("c", 3);

    assert!(cache.get(&"b").is_none());
    assert_eq!(cache.get(&"a"), Some(1));
    assert_eq!(cache.get(&"c"), Some(3));
}

#[test]
fn every_method_is_inert_when_no_tokio_runtime_is_available() {
    let cache = BlockingLruCache::new(NonZeroUsize::new(2).expect("capacity"));

    // Insert is silently dropped — reads stay empty.
    cache.insert("first", 1);
    assert!(cache.get(&"first").is_none());

    // get_or_insert_with still computes the value but never stores it.
    assert_eq!(cache.get_or_insert_with("first", || 2), 2);
    assert!(cache.get(&"first").is_none());

    // Remove and clear are no-ops that return `None` / unit.
    assert!(cache.remove(&"first").is_none());
    cache.clear();

    // with_mut hands the callback a throwaway cache that does not
    // survive the closure.
    let result = cache.with_mut(|inner| {
        inner.put("tmp", 3);
        inner.get(&"tmp").cloned()
    });
    assert_eq!(result, Some(3));
    assert!(cache.get(&"tmp").is_none());

    // blocking_lock returns `None` when no runtime is present.
    assert!(cache.blocking_lock().is_none());
}
