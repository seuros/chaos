use super::*;

#[test]
fn provider_tag_classification_covers_known_hosts() {
    assert_eq!(openai_compatible_provider_tag("https://api.x.ai/v1"), "xai");
    assert_eq!(
        openai_compatible_provider_tag("https://api.groq.com/openai/v1"),
        "groq"
    );
    assert_eq!(
        openai_compatible_provider_tag("https://api.openai.com/v1"),
        "openai"
    );
    // Unknown host: still classified as openai-compatible, since
    // base_url carries the disambiguating identity.
    assert_eq!(
        openai_compatible_provider_tag("https://proxy.internal/v1"),
        "openai"
    );
}

#[test]
fn sniffer_for_returns_none_without_shared_store() {
    // This test runs in isolation from boot — no store installed.
    // The factory should degrade cleanly rather than panic.
    assert!(sniffer_for("anthropic_messages", "https://api.anthropic.com/v1").is_none());
    assert!(sniffer_for("chat_completions", "https://api.openai.com/v1").is_none());
    assert!(sniffer_for("unknown_wire", "https://example.com").is_none());
}
