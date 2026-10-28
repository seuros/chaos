use std::borrow::Borrow;
use std::hash::Hash;
use std::num::NonZeroUsize;

use lru::LruCache;
use sha1::Digest;
use sha1::Sha1;
use tokio::sync::Mutex;
use tokio::sync::MutexGuard;

/// A minimal LRU cache protected by a Tokio mutex.
/// Calls outside a Tokio runtime are no-ops.
pub struct BlockingLruCache<K, V> {
    inner: Mutex<LruCache<K, V>>,
}

impl<K, V> BlockingLruCache<K, V>
where
    K: Eq + Hash,
{
    /// Creates a cache that allocates as entries arrive, up to `capacity`.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            inner: Mutex::new(LruCache::sparse(capacity)),
        }
    }

    /// Returns a clone of the cached value for `key`, or computes and inserts it.
    pub fn get_or_insert_with(&self, key: K, value: impl FnOnce() -> V) -> V
    where
        V: Clone,
    {
        if let Some(mut guard) = lock_if_runtime(&self.inner) {
            return guard.get_or_insert(key, value).clone();
        }
        value()
    }

    /// Like `get_or_insert_with`, but the value factory may fail.
    pub fn get_or_try_insert_with<E>(
        &self,
        key: K,
        value: impl FnOnce() -> Result<V, E>,
    ) -> Result<V, E>
    where
        V: Clone,
    {
        if let Some(mut guard) = lock_if_runtime(&self.inner) {
            return guard.try_get_or_insert(key, value).cloned();
        }
        value()
    }

    /// Builds a cache if `capacity` is non-zero, returning `None` otherwise.
    #[must_use]
    pub fn try_with_capacity(capacity: usize) -> Option<Self> {
        NonZeroUsize::new(capacity).map(Self::new)
    }

    /// Returns a clone of the cached value corresponding to `key`, if present.
    pub fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        V: Clone,
    {
        let mut guard = lock_if_runtime(&self.inner)?;
        guard.get(key).cloned()
    }

    /// Inserts `value` for `key`, returning the previous entry if it existed.
    pub fn insert(&self, key: K, value: V) -> Option<V> {
        let mut guard = lock_if_runtime(&self.inner)?;
        guard.put(key, value)
    }

    /// Removes the entry for `key` if it exists, returning it.
    pub fn remove<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let mut guard = lock_if_runtime(&self.inner)?;
        guard.pop(key)
    }

    /// Clears all entries from the cache.
    pub fn clear(&self) {
        if let Some(mut guard) = lock_if_runtime(&self.inner) {
            guard.clear();
        }
    }

    /// Executes `callback` with a mutable reference to the underlying cache.
    pub fn with_mut<R>(&self, callback: impl FnOnce(&mut LruCache<K, V>) -> R) -> R {
        if let Some(mut guard) = lock_if_runtime(&self.inner) {
            callback(&mut guard)
        } else {
            let mut disabled = LruCache::unbounded();
            callback(&mut disabled)
        }
    }

    /// Provides direct access to the cache guard when a Tokio runtime is available.
    pub fn blocking_lock(&self) -> Option<MutexGuard<'_, LruCache<K, V>>> {
        lock_if_runtime(&self.inner)
    }
}

fn lock_if_runtime<K, V>(m: &Mutex<LruCache<K, V>>) -> Option<MutexGuard<'_, LruCache<K, V>>>
where
    K: Eq + Hash,
{
    tokio::runtime::Handle::try_current().ok()?;
    Some(tokio::task::block_in_place(|| m.blocking_lock()))
}

/// Computes the SHA-1 digest of `bytes`.
///
/// Useful for content-based cache keys when you want to avoid staleness
/// caused by path-only keys.
#[must_use]
pub fn sha1_digest(bytes: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    let mut out = [0; 20];
    out.copy_from_slice(&result);
    out
}

#[cfg(test)]
mod tests;
