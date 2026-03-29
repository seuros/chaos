//! Chaos Parrot — provider adapters for LLM backends.
//!
//! Each LLM provider gets a parrot that translates chaos-abi into the
//! provider's wire format and back. Provider-agnostic by design — the
//! kernel speaks chaos-abi, parrots handle the dialects.
//!
//! Built with ABI and hooks. No poker.

pub mod anthropic;

use chaos_abi::ModelAdapter;

/// Select the adapter for a provider by its wire format identifier.
///
/// Returns `None` if the wire format is not (yet) handled by parrot.
/// The kernel falls back to its existing direct path for unhandled
/// providers.
pub fn adapter_for_wire(
    wire: &str,
    base_url: String,
    api_key: String,
    default_model: Option<String>,
) -> Option<Box<dyn ModelAdapter>> {
    match wire {
        "anthropic_messages" => Some(Box::new(anthropic::AnthropicAdapter::new(
            base_url,
            api_key,
            default_model,
        ))),
        // "responses" stays on the existing codex-api path for now.
        // "chat_completions" is next.
        _ => None,
    }
}
