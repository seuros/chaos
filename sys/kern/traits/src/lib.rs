//! Narrow trait abstractions for decoupling codex-core orchestration into satellite crates.
//!
//! Each trait defines the minimal surface a consumer needs. Core implements these traits on its
//! concrete types (Session, Config, etc.). Satellite crates depend on `chaos-traits` instead of
//! `codex-core`, breaking the circular dependency.

pub mod agent;
pub mod catalog;
pub mod config;
pub mod event_bus;
pub mod model;
pub mod state_access;
pub mod telemetry;

// Re-export traits at crate root for convenience.
pub use agent::{AgentSpawnConfig, AgentSpawner};
pub use config::{ConciergeConfig, MementoConfig, RolloutConfig};
pub use event_bus::EventEmitter;
pub use model::{ModelSampler, SamplingMessage, SamplingRequest, SamplingResponse};
pub use state_access::StateAccess;
pub use telemetry::TelemetrySource;
