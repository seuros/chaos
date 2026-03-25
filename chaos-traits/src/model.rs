//! Model sampling trait — narrow interface for running LLM inference without depending on the
//! full ModelClient/ModelClientSession machinery.

use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::TokenUsage;
use serde_json::Value;
use std::future::Future;

/// A single message in a sampling request.
#[derive(Clone, Debug)]
pub struct SamplingMessage {
    pub role: String,
    pub content: String,
}

/// Parameters for a model sampling request.
#[derive(Clone, Debug)]
pub struct SamplingRequest {
    pub model: String,
    pub instructions: String,
    pub input: Vec<SamplingMessage>,
    pub output_schema: Option<Value>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_summary: ReasoningSummary,
    pub service_tier: Option<ServiceTier>,
    pub turn_metadata_header: Option<String>,
}

/// Result of a model sampling request.
#[derive(Clone, Debug, Default)]
pub struct SamplingResponse {
    pub output_text: Option<String>,
    pub token_usage: Option<TokenUsage>,
}

/// Narrow interface for running a single model inference round-trip.
pub trait ModelSampler: Send + Sync {
    fn sample(
        &self,
        request: SamplingRequest,
    ) -> impl Future<Output = anyhow::Result<SamplingResponse>> + Send;
}
