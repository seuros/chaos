//! Provider-neutral reasoning configuration.

use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::openai_models::ReasoningEffort;

/// Reasoning controls for models that support chain-of-thought.
///
/// Each adapter maps these to the provider's specific parameters:
/// - **OpenAI**: `reasoning.effort` + `reasoning.summary`
/// - **Anthropic**: `thinking.type` = `"enabled"`, `thinking.budget_tokens`
///   derived from effort level
#[derive(Debug, Clone, PartialEq)]
pub struct ReasoningConfig {
    /// How much effort the model should spend reasoning.
    pub effort: Option<ReasoningEffort>,

    /// How to summarize the reasoning in the response.
    pub summary: Option<ReasoningSummary>,
}
