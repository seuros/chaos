//! Test support: keep vault keys in memory instead of the OS keyring.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::PoisonError;

use custody_machines::stores::Source;
pub use custody_machines::stores::testing::MemorySource;

use super::VaultSources;

static ISOLATED: LazyLock<Mutex<Option<HashMap<PathBuf, MemorySource>>>> =
    LazyLock::new(Mutex::default);

/// From now on, every [`crate::LocalSecretsBackend::shared`] vault in this
/// process keeps its key in memory, one key per home, never in the OS keyring.
pub fn isolate() {
    ISOLATED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(HashMap::new);
}

/// A ladder whose only usable source is `key`; the rest answer empty and are
/// never written.
pub fn sources(key: &MemorySource) -> VaultSources {
    std::array::from_fn(|index| -> Box<dyn Source> {
        if index == 0 {
            Box::new(key.clone())
        } else {
            Box::new(MemorySource::provisioned(format!("unused source {index}")))
        }
    })
}

pub(crate) fn isolated_sources(chaos_home: &Path) -> Option<VaultSources> {
    let mut isolated = ISOLATED.lock().unwrap_or_else(PoisonError::into_inner);
    let keys = isolated.as_mut()?;
    let key = keys
        .entry(chaos_home.to_path_buf())
        .or_insert_with(|| MemorySource::durable("isolated test key"));
    Some(sources(key))
}
