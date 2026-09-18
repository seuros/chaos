use std::sync::LazyLock;

use chaos_vfs::VfsKind;
use tokio::sync::watch;

/// Process-wide health for the interactive session journal sink.
///
/// The top bar subscribes to changes so background writer failures are visible
/// even when the UI is otherwise idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceHealth {
    Healthy,
    Degraded,
    Failing,
    Failed,
}

pub fn persistence_health() -> PersistenceHealth {
    PERSISTENCE_STATUS.borrow().health
}

pub(crate) fn set_persistence_health(health: PersistenceHealth) {
    update_status(&PERSISTENCE_STATUS, |status| status.health = health);
}

/// Process-wide runtime-storage backend selected by the kernel.
///
/// The top bar reads this directly so the UI shows whether persistence is
/// currently backed by SQLite or Postgres without having to reopen storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStorageBackend {
    Sqlite,
    Postgres,
}

impl From<VfsKind> for RuntimeStorageBackend {
    fn from(kind: VfsKind) -> Self {
        match kind {
            VfsKind::Sqlite => Self::Sqlite,
            VfsKind::Postgres => Self::Postgres,
        }
    }
}

pub fn runtime_storage_backend() -> RuntimeStorageBackend {
    PERSISTENCE_STATUS.borrow().backend
}

pub(crate) fn set_runtime_storage_backend(backend: RuntimeStorageBackend) {
    update_status(&PERSISTENCE_STATUS, |status| status.backend = backend);
}

/// Latest process-wide persistence state, available without reopening storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistenceStatus {
    pub health: PersistenceHealth,
    pub backend: RuntimeStorageBackend,
}

impl Default for PersistenceStatus {
    fn default() -> Self {
        Self {
            health: PersistenceHealth::Healthy,
            backend: RuntimeStorageBackend::Sqlite,
        }
    }
}

static PERSISTENCE_STATUS: LazyLock<watch::Sender<PersistenceStatus>> =
    LazyLock::new(|| watch::channel(PersistenceStatus::default()).0);

/// Subscribe to actual health/backend changes, retaining the current snapshot.
///
/// Read the initial value with `borrow`; subsequent `changed` notifications can
/// be coalesced, so consumers should always read the latest value. Identical
/// writes do not wake subscribers, and updates survive periods with no listeners.
pub fn subscribe_persistence_status() -> watch::Receiver<PersistenceStatus> {
    PERSISTENCE_STATUS.subscribe()
}

fn update_status(
    sender: &watch::Sender<PersistenceStatus>,
    update: impl FnOnce(&mut PersistenceStatus),
) {
    sender.send_if_modified(|status| {
        let previous = *status;
        update(status);
        *status != previous
    });
}

#[cfg(test)]
mod tests;
