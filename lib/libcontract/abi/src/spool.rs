//! Spool: durable, provider-agnostic apprentice-job lifecycle.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::OnceLock;

use serde::Deserialize;
use serde::Serialize;
use state_machines::state_machine;

use crate::TurnRequest;
use crate::turn_result::TurnResult;

state_machine! {
    name: Spool,
    dynamic: true,
    initial: Queued,
    states: [Queued, InProgress, Completed, Failed, Expired, Cancelled],
    events {
        submit {
            transition: { from: Queued, to: InProgress }
        }
        submit_failed {
            transition: { from: Queued, to: Failed }
        }
        poll {
            transition: { from: InProgress, to: InProgress }
        }
        finish {
            transition: { from: InProgress, to: Completed }
        }
        fail {
            transition: { from: InProgress, to: Failed }
        }
        expire {
            transition: { from: InProgress, to: Expired }
        }
        cancel {
            transition: { from: Queued, to: Cancelled }
            transition: { from: InProgress, to: Cancelled }
        }
    }
}

/// Row-level data persisted alongside the machine state in `spool_jobs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpoolRecord {
    pub manifest_id: String,
    pub backend: String,
    pub batch_id: Option<String>,
    pub request_count: u32,
    pub error: Option<String>,
    pub submitted_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl SpoolRecord {
    pub fn is_terminal(state: &str) -> bool {
        matches!(state, "Completed" | "Failed" | "Expired" | "Cancelled")
    }
}

/// Coarse per-poll phase reported by backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpoolPhase {
    InProgress,
    Completed,
    Failed,
    Expired,
    Cancelled,
}

/// One poll tick's worth of information.
#[derive(Debug, Clone)]
pub struct SpoolStatusReport {
    pub phase: SpoolPhase,
    pub raw_provider_status: String,
}

/// Transport / lifecycle failures from a [`SpoolBackend`].
#[derive(Debug, thiserror::Error)]
pub enum SpoolError {
    #[error("backend does not support this operation")]
    NotSupported,

    #[error("authentication failed")]
    Auth,

    #[error("rate limited{}", retry_after.map(|s| format!(" (retry in {s}s)")).unwrap_or_default())]
    RateLimit { retry_after: Option<u64> },

    #[error("provider error {status}: {message}")]
    ProviderError { status: u16, message: String },

    #[error("translation error: {0}")]
    Translation(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

/// Per-item result keyed by the caller-supplied `custom_id`.
pub type SpoolItem = (String, TurnResult);

/// Provider-agnostic apprentice job substrate.
pub trait SpoolBackend: Send + Sync {
    /// Backend name used in `spool_jobs.backend` and logs.
    fn name(&self) -> &'static str;

    /// Submit canonical turn requests; returns the provider's batch id.
    fn submit(
        &self,
        items: Vec<(String, TurnRequest)>,
    ) -> Pin<Box<dyn Future<Output = Result<String, SpoolError>> + Send + '_>>;

    /// Poll the batch.
    fn poll(
        &self,
        batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<SpoolStatusReport, SpoolError>> + Send + '_>>;

    /// Fetch completed results as canonical `TurnResult`s.
    fn fetch_results(
        &self,
        batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SpoolItem>, SpoolError>> + Send + '_>>;

    /// Best-effort cancel; may no-op when already terminal.
    fn cancel(
        &self,
        batch_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SpoolError>> + Send + '_>>;
}

/// Raw provider payload (JSON / JSONL) persisted in `spool_jobs.raw_result`.
#[derive(Debug, Clone)]
pub struct SpoolCheckpoint {
    pub manifest_id: String,
    pub batch_id: String,
    pub body: String,
}

/// Name-keyed lookup of registered [`SpoolBackend`] implementations.
///
/// Keys match [`SpoolBackend::name`] (`"anthropic"`, `"xai"`, ...).
#[derive(Clone, Default)]
pub struct SpoolRegistry {
    backends: HashMap<String, Arc<dyn SpoolBackend>>,
}

impl SpoolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a backend keyed by its [`SpoolBackend::name`].
    pub fn register(&mut self, backend: Arc<dyn SpoolBackend>) {
        self.backends.insert(backend.name().to_string(), backend);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn SpoolBackend>> {
        self.backends.get(name).cloned()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.backends.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }
}

impl std::fmt::Debug for SpoolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpoolRegistry")
            .field("backends", &self.backends.keys().collect::<Vec<_>>())
            .finish()
    }
}

static SHARED_REGISTRY: OnceLock<Arc<SpoolRegistry>> = OnceLock::new();

/// Install the process-wide spool registry. Returns `Err(registry)` if a
/// registry is already installed — kernel boot is the only caller and it
/// runs exactly once.
pub fn set_shared_spool_registry(registry: Arc<SpoolRegistry>) -> Result<(), Arc<SpoolRegistry>> {
    SHARED_REGISTRY.set(registry)
}

/// Fetch the process-wide spool registry if one has been installed.
/// Returns `None` in tests that spin up without a registry, or when no
/// backends are configured via env.
pub fn shared_spool_registry() -> Option<Arc<SpoolRegistry>> {
    SHARED_REGISTRY.get().cloned()
}
