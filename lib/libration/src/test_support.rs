//! Test fixtures for libration test suites.

use crate::UsageStore;
use chaos_storage::ChaosStorageProvider;
use std::sync::Arc;

/// Open a temporary SQLite database for testing, initialize the runtime
/// schema, and return a (tempdir, store) tuple. The store is connected to
/// the temporary database and ready for use; the returned tempdir keeps
/// the database alive for the lifetime of the test.
pub async fn open_sqlite_store() -> (tempfile::TempDir, Arc<UsageStore>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = chaos_proc::open_runtime_db(dir.path())
        .await
        .expect("open runtime db");
    let provider = ChaosStorageProvider::from_sqlite_pool(pool);
    let store = UsageStore::from_provider(&provider).expect("store");
    (dir, store)
}
