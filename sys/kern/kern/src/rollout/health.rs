use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

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
