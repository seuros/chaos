//! Shared-store access and sniffer factory for the ration pipeline.
//!
//! The previous revision of this module kept a process-global
//! `HashMap<host, Arc<UsageSniffer>>` that transport code silently
//! consulted whenever it wanted to record a response. That shape made
//! the dependency invisible at call sites, keyed on URL host (which
//! can't distinguish two configs that share a host but not a base
//! URL), and turned sniffer installation into magic that had to be
//! globally ordered with adapter construction.
//!
//! This module still keeps a single well-known reference — the
//! `UsageStore` that every persisted snapshot ultimately lands in —
//! but transport-level sniffing is now wired explicitly. Adapter
//! constructors accept an `Option<Arc<UsageSniffer>>`, and the
//! [`sniffer_for`] helper below builds the right extractor for a given
//! wire-format + base-URL pair.

use crate::AnthropicHeaders;
use crate::OpenAICompatibleHeaders;
use crate::UsageSniffer;
use crate::UsageStore;
use std::sync::Arc;
use std::sync::OnceLock;

static SHARED_STORE: OnceLock<Arc<UsageStore>> = OnceLock::new();

/// Install the process-wide usage store. Returns `Err(store)` if a
/// store is already installed — kernel boot code is the only caller
/// and it runs exactly once.
pub fn set_shared_store(store: Arc<UsageStore>) -> Result<(), Arc<UsageStore>> {
    SHARED_STORE.set(store)
}

/// Fetch the process-wide usage store if one has been installed.
/// Intended for TUI/status surfaces that want to read `latest_*`
/// without threading an `Arc<UsageStore>` from boot.
pub fn shared_store() -> Option<Arc<UsageStore>> {
    SHARED_STORE.get().cloned()
}

/// Build a [`UsageSniffer`] suited to the given wire format and base
/// URL, backed by the process-wide shared store. Returns `None` when
/// no store has been installed yet (typical in unit tests that spin up
/// adapters without the runtime database) or when the wire format has
/// no known extractor — callers should treat that as "ration off" and
/// carry on without recording.
pub fn sniffer_for(wire: &str, base_url: &str) -> Option<Arc<UsageSniffer>> {
    let store = shared_store()?;
    let sniffer = match wire {
        "anthropic_messages" => UsageSniffer::new(AnthropicHeaders, base_url.to_string(), store),
        "chat_completions" | "responses" | "tensorzero" => {
            let provider_tag = openai_compatible_provider_tag(base_url);
            UsageSniffer::new(
                OpenAICompatibleHeaders::new(provider_tag),
                base_url.to_string(),
                store,
            )
        }
        _ => return None,
    };
    Some(Arc::new(sniffer))
}

/// Classify an OpenAI-compatible endpoint by its host so counts get
/// attributed to the right provider tag. Unknown hosts fall back to
/// "openai" — `base_url` is already part of the snapshot's identity,
/// so the tag is only used for display and coarse-grained grouping.
fn openai_compatible_provider_tag(base_url: &str) -> &'static str {
    let lower = base_url.to_ascii_lowercase();
    if lower.contains("api.x.ai") {
        "xai"
    } else if lower.contains("api.groq.com") {
        "groq"
    } else {
        "openai"
    }
}

#[cfg(test)]
mod tests;
