//! Kernel-provided settings persistence.
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

pub type PersistenceFuture<'a, T> = Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send + 'a>>;

#[derive(Clone, Debug)]
pub struct SettingsSnapshot {
    pub revision: i64,
    pub settings: toml::Value,
}

pub trait SettingsPersistence: Send + Sync {
    fn snapshot<'a>(&'a self, home: &'a Path) -> PersistenceFuture<'a, SettingsSnapshot>;
    fn commit<'a>(
        &'a self,
        home: &'a Path,
        revision: i64,
        settings: toml::Value,
    ) -> PersistenceFuture<'a, ()>;
}

static PERSISTENCE: OnceLock<Arc<dyn SettingsPersistence>> = OnceLock::new();

/// Installation is idempotent: every kernel entry point uses the same adapter.
pub fn install(backend: Arc<dyn SettingsPersistence>) {
    let _ = PERSISTENCE.set(backend);
}

pub fn backend() -> anyhow::Result<&'static dyn SettingsPersistence> {
    PERSISTENCE
        .get()
        .map(Arc::as_ref)
        .ok_or_else(|| anyhow::anyhow!("settings persistence is not initialized"))
}
