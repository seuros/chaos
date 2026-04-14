//! The canonical request the core harness sends to any model adapter.

use crate::reasoning::ReasoningConfig;
use crate::tools::ToolDef;
use chaos_ipc::config_types::Verbosity;
use chaos_ipc::models::ResponseItem;
use serde_json::Map;
use serde_json::Value;
use std::sync::Arc;
use std::sync::OnceLock;

/// A provider-agnostic turn request.
///
/// The core builds this; each [`ModelAdapter`](crate::ModelAdapter)
/// translates it into the provider's wire format.
#[derive(Debug, Clone)]
pub struct TurnRequest {
    /// Model identifier (provider-specific slug).
    pub model: String,

    /// System-level instructions (system prompt).
    pub instructions: String,

    /// Conversation history as neutral items.
    pub input: Vec<ResponseItem>,

    /// Tool definitions in provider-neutral format.
    pub tools: Vec<ToolDef>,

    /// Whether the model may invoke multiple tools in parallel.
    pub parallel_tool_calls: bool,

    /// Reasoning configuration (effort level + summary mode).
    pub reasoning: Option<ReasoningConfig>,

    /// Optional structured output schema (JSON Schema as value).
    pub output_schema: Option<Value>,

    /// Verbosity hint for models that support it.
    pub verbosity: Option<Verbosity>,

    /// Optional per-turn state slot used by transports that need to cache a
    /// sticky routing token across multiple requests within the same turn.
    pub turn_state: Option<Arc<OnceLock<String>>>,

    /// Provider-opaque extension bag.
    ///
    /// The core populates this from config; adapters read what they need
    /// and ignore the rest. This avoids adding provider-specific fields
    /// to the ABI struct.
    ///
    /// Examples:
    /// - OpenAI: `store`, `include`, `service_tier`, `prompt_cache_key`
    /// - Anthropic: `max_tokens`, `thinking.budget_tokens`
    pub extensions: Map<String, Value>,
}
