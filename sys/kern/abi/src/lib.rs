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
pub mod stream;
pub mod tools;

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
pub use stream::TurnStream;
pub use tools::FreeformToolDef;
pub use tools::FunctionToolDef;
pub use tools::ToolDef;

// Re-export neutral types from protocol that the ABI traffics in.
pub use chaos_ipc::config_types::ReasoningSummary;
pub use chaos_ipc::config_types::Verbosity;
pub use chaos_ipc::models::ContentItem;
pub use chaos_ipc::models::ResponseItem;
pub use chaos_ipc::openai_models::ReasoningEffort;
pub use chaos_ipc::protocol::RateLimitSnapshot;
pub use chaos_ipc::protocol::TokenUsage;
