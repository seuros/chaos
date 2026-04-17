//! Chaos-ABI: the provider-agnostic binary interface for model adapters.
//!
//! The Chaos core (harness) speaks this ABI. Each model provider (OpenAI,
//! Anthropic, etc.) implements [`ModelAdapter`] to translate between the
//! ABI and its own wire format.
//!
//! No provider is privileged — all are equidistant from the ABI.

pub mod adapter;
pub mod error;
pub mod event;
pub mod models;
pub mod reasoning;
pub mod request;
pub mod secret;
pub mod spool;
pub mod stream;
pub mod tools;
pub mod turn_result;

// Re-export ABI surface.
pub use adapter::AdapterFuture;
pub use adapter::ModelAdapter;
pub use error::AbiError;
pub use event::TurnEvent;
pub use models::AbiModelInfo;
pub use models::AdapterCapabilities;

pub use models::ListModelsError;
pub use models::ListModelsFuture;
pub use reasoning::ReasoningConfig;
pub use request::TurnRequest;
pub use secret::Secret;
pub use spool::DynamicSpool;
pub use spool::SpoolBackend;
pub use spool::SpoolCheckpoint;
pub use spool::SpoolError;
pub use spool::SpoolEvent;
pub use spool::SpoolItem;
pub use spool::SpoolPhase;
pub use spool::SpoolRecord;
pub use spool::SpoolRegistry;
pub use spool::SpoolStatusReport;
pub use spool::set_shared_spool_registry;
pub use spool::shared_spool_registry;
pub use stream::TurnStream;
pub use tools::FreeformToolDef;
pub use tools::FunctionToolDef;
pub use tools::ToolDef;
pub use turn_result::TurnError;
pub use turn_result::TurnOutput;
pub use turn_result::TurnResult;

// Re-export neutral types from protocol that the ABI traffics in.
pub use chaos_ipc::config_types::ReasoningSummary;
pub use chaos_ipc::config_types::Verbosity;
pub use chaos_ipc::models::ContentItem;
pub use chaos_ipc::models::ResponseItem;
pub use chaos_ipc::openai_models::ReasoningEffort;
pub use chaos_ipc::protocol::RateLimitSnapshot;
pub use chaos_ipc::protocol::TokenUsage;
