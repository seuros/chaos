//! Chaos Parrot — provider adapters and wire-format clients for LLM backends.
//!
//! The kernel should not speak provider wire formats directly. `chaos-parrot`
//! owns those dialects:
//!
//! - provider-neutral turn adapters implementing `chaos-abi::ModelAdapter`
//! - wire-format clients/helpers for provider-specific endpoints
//!
//! Built with ABI and hooks. No poker.

pub mod adapter;
pub mod anthropic;
pub mod auth;
pub mod common;
pub mod endpoint;
pub mod error;
pub mod openai;
pub mod provider;
pub mod rate_limits;
pub mod requests;
pub mod sanitize;
pub mod sse;
pub mod telemetry;

use chaos_abi::ModelAdapter;

pub use crate::auth::AuthProvider;
pub use crate::common::CompactionInput;
pub use crate::common::MemorySummarizeInput;
pub use crate::common::MemorySummarizeOutput;
pub use crate::common::RawMemory;
pub use crate::common::RawMemoryMetadata;
pub use crate::common::ResponseCreateWsRequest;
pub use crate::common::ResponseEvent;
pub use crate::common::ResponseStream;
pub use crate::common::ResponsesApiRequest;
pub use crate::common::create_text_param_for_request;
pub use crate::endpoint::compact::CompactClient;
pub use crate::endpoint::memories::MemoriesClient;
pub use crate::endpoint::models::ModelsClient;
pub use crate::endpoint::responses::ResponsesClient;
pub use crate::endpoint::responses::ResponsesOptions;
pub use crate::endpoint::responses_websocket::ResponsesWebsocketClient;
pub use crate::endpoint::responses_websocket::ResponsesWebsocketConnection;
pub use crate::error::ApiError;
pub use crate::provider::Provider;
pub use crate::provider::is_azure_responses_wire_base_url;
pub use crate::requests::headers::build_conversation_headers;
pub use crate::sse::stream_from_fixture;
pub use crate::telemetry::SseTelemetry;
pub use crate::telemetry::WebsocketTelemetry;
pub use codex_client::RamaTransport;
pub use codex_client::RequestTelemetry;
pub use codex_client::TransportError;

/// Select the adapter for a provider by its wire format identifier.
///
/// Returns `None` if the wire format is not yet handled by parrot.
pub fn adapter_for_wire(
    wire: &str,
    base_url: String,
    api_key: String,
    default_model: Option<String>,
) -> Option<Box<dyn ModelAdapter>> {
    match wire {
        "anthropic_messages" => Some(Box::new(
            anthropic::AnthropicAdapter::from_base_url_and_api_key(
                base_url,
                api_key,
                default_model,
            ),
        )),
        "responses" => Some(Box::new(openai::OpenAiAdapter::from_base_url_and_api_key(
            base_url,
            api_key,
            default_model,
        ))),
        _ => None,
    }
}
