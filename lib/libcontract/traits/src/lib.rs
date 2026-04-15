//! Narrow trait abstractions for decoupling chaos-kern orchestration into satellite crates.
//!
//! Each trait defines the minimal surface a consumer needs. Core implements these traits on its
//! concrete types (Session, Config, etc.). Satellite crates depend on `chaos-traits` instead of
//! `chaos-kern`, breaking the circular dependency.

pub mod agent;
pub mod catalog;
pub mod config;
pub mod event_bus;
pub mod model;
pub mod router;
pub mod runtime_access;
pub mod telemetry;

// Re-export traits at crate root for convenience.
pub use agent::AgentSpawnConfig;
pub use agent::AgentSpawner;
pub use catalog::McpCatalogSink;
pub use config::ConciergeConfig;
pub use config::MementoConfig;
pub use config::RolloutConfig;
pub use event_bus::EventEmitter;
pub use model::ModelSampler;
pub use model::SamplingMessage;
pub use model::SamplingRequest;
pub use model::SamplingResponse;
pub use router::Adapter;
pub use router::AdapterError;
pub use router::DEFAULT_ADAPTER_CAPACITY;
pub use router::Packet;
pub use router::Router;
pub use router::Tunnel;
pub use router::enumerate_traced;
pub use runtime_access::RuntimeAccess;
pub use telemetry::TelemetrySource;
