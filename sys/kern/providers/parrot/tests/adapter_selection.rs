#[test]
fn responses_wire_selects_the_provider_dialect_without_substring_matching() {
    for (url, expected) in [
        ("https://api.x.ai/v1", "xAI"),
        ("https://cli-chat-proxy.grok.com/v1", "xAI"),
        ("https://api.openai.com/v1", "OpenAI"),
        ("https://api.moonshot.ai/v1", "OpenAI"),
        ("https://api.x.ai.example.com/v1", "OpenAI"),
        ("https://example.com/api.x.ai", "OpenAI"),
        ("https://cli-chat-proxy.grok.com.example.com/v1", "OpenAI"),
    ] {
        let adapter =
            chaos_parrot::adapter_for_wire("responses", url.to_owned(), String::new(), None)
                .expect("Responses adapter");
        assert_eq!(adapter.provider_name(), expected, "{url}");
    }
}
