use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

use chaos_storage::StorageKind;

/// Process-wide health for the interactive session journal sink.
///
/// The top bar reads this directly so persistence failures are visible even
/// when they happen on the background rollout writer task and are otherwise
/// only logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceHealth {
    Healthy,
    Degraded,
    Failing,
    Failed,
}

const HEALTHY: u8 = 0;
const DEGRADED: u8 = 1;
const FAILING: u8 = 2;
const FAILED: u8 = 3;

static PERSISTENCE_HEALTH: AtomicU8 = AtomicU8::new(HEALTHY);

impl PersistenceHealth {
    fn as_u8(self) -> u8 {
        match self {
            Self::Healthy => HEALTHY,
            Self::Degraded => DEGRADED,
            Self::Failing => FAILING,
            Self::Failed => FAILED,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            DEGRADED => Self::Degraded,
            FAILING => Self::Failing,
            FAILED => Self::Failed,
            _ => Self::Healthy,
        }
    }
}

pub fn persistence_health() -> PersistenceHealth {
    PersistenceHealth::from_u8(PERSISTENCE_HEALTH.load(Ordering::Relaxed))
}

pub(crate) fn set_persistence_health(health: PersistenceHealth) {
    PERSISTENCE_HEALTH.store(health.as_u8(), Ordering::Relaxed);
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

const STORAGE_SQLITE: u8 = 0;
const STORAGE_POSTGRES: u8 = 1;

static RUNTIME_STORAGE_BACKEND: AtomicU8 = AtomicU8::new(STORAGE_SQLITE);

impl RuntimeStorageBackend {
    fn as_u8(self) -> u8 {
        match self {
            Self::Sqlite => STORAGE_SQLITE,
            Self::Postgres => STORAGE_POSTGRES,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            STORAGE_POSTGRES => Self::Postgres,
            _ => Self::Sqlite,
        }
    }
}

impl From<StorageKind> for RuntimeStorageBackend {
    fn from(kind: StorageKind) -> Self {
        match kind {
            StorageKind::Sqlite => Self::Sqlite,
            StorageKind::Postgres => Self::Postgres,
        }
    }
}

pub fn runtime_storage_backend() -> RuntimeStorageBackend {
    RuntimeStorageBackend::from_u8(RUNTIME_STORAGE_BACKEND.load(Ordering::Relaxed))
}

pub(crate) fn set_runtime_storage_backend(backend: RuntimeStorageBackend) {
    RUNTIME_STORAGE_BACKEND.store(backend.as_u8(), Ordering::Relaxed);
}
