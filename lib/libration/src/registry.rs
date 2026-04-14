//! Process-global registry mapping a URL's host to the `UsageSniffer`
//! that should handle its responses.
//!
//! Kernel boot code calls [`install`] once per configured provider, then
//! transport code looks up the sniffer for an outgoing URL with
//! [`lookup`] without needing to thread anything through intermediate
//! call stacks.

use crate::AnthropicHeaders;
use crate::OpenAICompatibleHeaders;
use crate::UsageSniffer;
use crate::UsageStore;
use chaos_storage::ChaosStorageProvider;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::RwLock;

static REGISTRY: OnceLock<RwLock<HashMap<String, Arc<UsageSniffer>>>> = OnceLock::new();

fn registry() -> &'static RwLock<HashMap<String, Arc<UsageSniffer>>> {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register a sniffer under a host (e.g. `"api.openai.com"`). Subsequent
/// registrations with the same host replace the previous sniffer.
pub fn install(host: impl Into<String>, sniffer: Arc<UsageSniffer>) {
    if let Ok(mut map) = registry().write() {
        map.insert(host.into().to_ascii_lowercase(), sniffer);
    }
}

/// Look up the sniffer for an outgoing URL. Returns `None` when no
/// sniffer has been registered for that host, so transports fall back
/// to the no-op path without needing to know about ration.
pub fn lookup(url: &str) -> Option<Arc<UsageSniffer>> {
    let host = host_from_url(url)?.to_ascii_lowercase();
    let map = registry().read().ok()?;
    map.get(&host).cloned()
}

/// Install the default set of sniffers for chaos's first-party providers
/// (OpenAI, xAI, Groq, Anthropic) against the given storage provider.
/// Safe to call multiple times — later calls replace earlier sniffers.
///
/// Returns the shared UsageStore so kernel code can also query it directly
/// for the "85% left" surface in the TUI.
pub fn install_default_sniffers(storage: &ChaosStorageProvider) -> Option<Arc<UsageStore>> {
    let store = Arc::new(UsageStore::from_provider(storage)?);

    let openai = Arc::new(UsageSniffer::new(
        OpenAICompatibleHeaders::new("openai"),
        "https://api.openai.com/v1",
        Arc::clone(&store),
    ));
    install("api.openai.com", openai);

    let xai = Arc::new(UsageSniffer::new(
        OpenAICompatibleHeaders::new("xai"),
        "https://api.x.ai/v1",
        Arc::clone(&store),
    ));
    install("api.x.ai", xai);

    let groq = Arc::new(UsageSniffer::new(
        OpenAICompatibleHeaders::new("groq"),
        "https://api.groq.com/openai/v1",
        Arc::clone(&store),
    ));
    install("api.groq.com", groq);

    let anthropic = Arc::new(UsageSniffer::new(
        AnthropicHeaders,
        "https://api.anthropic.com/v1",
        Arc::clone(&store),
    ));
    install("api.anthropic.com", anthropic);

    Some(store)
}

fn host_from_url(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split('/').next()?;
    // Strip userinfo and port.
    let authority = authority
        .rsplit_once('@')
        .map(|(_, a)| a)
        .unwrap_or(authority);
    authority.split(':').next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_extraction_covers_common_shapes() {
        assert_eq!(
            host_from_url("https://api.openai.com/v1/chat"),
            Some("api.openai.com")
        );
        assert_eq!(host_from_url("http://api.x.ai:8080/v1"), Some("api.x.ai"));
        assert_eq!(
            host_from_url("https://user:pass@api.anthropic.com/v1"),
            Some("api.anthropic.com")
        );
        assert_eq!(host_from_url("api.groq.com/v1"), Some("api.groq.com"));
    }
}
