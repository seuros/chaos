//! Provider-neutral model metadata and discovery.
//!
//! Each adapter translates its wire format's model listing into
//! `AbiModelInfo`. The kernel works with this neutral type and
//! maps it into its own `ModelInfo` when needed.
//!
//! Discovery is an optional adapter capability. Providers that
//! do not expose a `/models` endpoint return `Unsupported`.

use serde::Deserialize;
use serde::Serialize;

/// Provider-neutral model metadata.
///
/// This is the minimal set of fields the kernel needs to route
/// requests, enforce limits, and populate the model picker. Each
/// adapter maps its provider's wire format into this struct.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AbiModelInfo {
    /// Model identifier used in API requests (e.g. `claude-sonnet-4-20250514`).
    pub id: String,

    /// Human-readable display name (e.g. `Claude Sonnet 4`).
    pub display_name: String,

    /// Maximum input tokens the model accepts.
    pub max_input_tokens: Option<i64>,

    /// Maximum output tokens the model can produce.
    pub max_output_tokens: Option<i64>,

    /// Whether the model supports thinking/reasoning.
    pub supports_thinking: bool,

    /// Whether the model supports image input.
    pub supports_images: bool,

    /// Whether the model supports structured output / JSON schema.
    pub supports_structured_output: bool,

    /// Whether the model supports reasoning effort levels.
    pub supports_reasoning_effort: bool,
}

/// Errors from optional model discovery.
///
/// This is intentionally separate from `AbiError` because discovery
/// failure is not a turn failure — the kernel should fall back to
/// cached or bundled metadata rather than aborting the session.
#[derive(Debug, thiserror::Error)]
pub enum ListModelsError {
    /// The adapter / provider endpoint does not support model listing.
    #[error("model listing not supported by this adapter")]
    Unsupported,

    /// The provider returned an error (auth, transport, etc.).
    #[error("discovery failed: {message}")]
    Failed { message: String },
}

/// Declares which optional capabilities an adapter instance supports.
///
/// The kernel checks this before calling optional methods so it can
/// skip network round-trips to providers that will 404 anyway.
#[derive(Debug, Clone, Default)]
pub struct AdapterCapabilities {
    /// Whether `list_models()` is implemented and the provider has
    /// a discovery endpoint.
    pub can_list_models: bool,
}

/// Future type for the async model listing.
pub type ListModelsFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<AbiModelInfo>, ListModelsError>> + Send + 'a>,
>;
