use super::*;

#[test]
fn detects_azure_responses_base_urls() {
    let positive_cases = [
        "https://foo.openai.azure.com/openai",
        "https://foo.openai.azure.us/openai/deployments/bar",
        "https://foo.cognitiveservices.azure.cn/openai",
        "https://foo.aoai.azure.com/openai",
        "https://foo.openai.azure-api.net/openai",
        "https://foo.z01.azurefd.net/",
    ];

    for base_url in positive_cases {
        assert!(
            is_azure_responses_wire_base_url("test", Some(base_url)),
            "expected {base_url} to be detected as Azure"
        );
    }

    assert!(is_azure_responses_wire_base_url(
        "Azure",
        Some("https://example.com")
    ));

    let negative_cases = [
        "https://api.openai.com/v1",
        "https://example.com/openai",
        "https://myproxy.azurewebsites.net/openai",
    ];

    for base_url in negative_cases {
        assert!(
            !is_azure_responses_wire_base_url("test", Some(base_url)),
            "expected {base_url} not to be detected as Azure"
        );
    }
}

#[test]
fn default_streaming_config_sets_common_retry_defaults() {
    let provider = Provider::from_base_url_with_default_streaming_config(
        "OpenAI",
        "https://example.test".into(),
        false,
    );

    assert_eq!(provider.name, "OpenAI");
    assert_eq!(provider.base_url, "https://example.test");
    assert!(provider.query_params.is_none());
    assert!(provider.headers.is_empty());
    assert_eq!(provider.retry.max_attempts, DEFAULT_PROVIDER_MAX_ATTEMPTS);
    assert_eq!(
        provider.retry.base_delay,
        Duration::from_millis(DEFAULT_PROVIDER_BASE_DELAY_MS)
    );
    assert!(!provider.retry.retry_429);
    assert!(provider.retry.retry_5xx);
    assert!(provider.retry.retry_transport);
    assert_eq!(
        provider.stream_idle_timeout,
        Duration::from_secs(DEFAULT_PROVIDER_STREAM_IDLE_TIMEOUT_SECS)
    );
}
