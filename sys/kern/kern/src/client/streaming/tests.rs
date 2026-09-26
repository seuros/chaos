use super::antigravity_model_slug;
use super::antigravity_usage_to_token_usage;
use super::clamp_usage_to_token_usage;
use chaos_clamp::AntigravityUsage;
use chaos_clamp::Usage;
use chaos_ipc::openai_models::ReasoningEffort;

#[test]
fn global_egress_disables_chatgpt_request_compression_for_dlp() {
    let auth = crate::ChaosAuth::create_dummy_chatgpt_auth_for_testing();
    for use_egress in [false, true] {
        let mut provider = crate::ModelProviderInfo::create_openai_provider(None);
        if use_egress {
            provider.egress =
                Some(chaos_client::Egress::parse("http://gateway/egress/chaos").unwrap());
        }
        let client = crate::client::ModelClient::new(
            None,
            chaos_ipc::ProcessId::new(),
            "openai".into(),
            provider,
            chaos_ipc::protocol::SessionSource::Exec,
            chaos_ipc::protocol::ApprovalPolicy::Headless,
            None,
            true,
            None,
            false,
            crate::config::ClampSettings::default(),
        );
        let compression = client
            .new_session()
            .responses_request_compression(Some(&auth));
        assert_eq!(matches!(compression, super::Compression::None), use_egress,);
    }
}

#[test]
fn antigravity_model_mapping_uses_observed_cli_slugs() {
    assert_eq!(
        antigravity_model_slug("gemini-3.1-pro-preview", Some(ReasoningEffort::Low)),
        "gemini-3.1-pro-low"
    );
    assert_eq!(
        antigravity_model_slug("google/gemini-3.1-pro", Some(ReasoningEffort::Medium)),
        "gemini-3.1-pro-high"
    );
    assert_eq!(
        antigravity_model_slug("gemini-3.6-flash", Some(ReasoningEffort::Medium)),
        "gemini-3.6-flash-medium"
    );
    assert_eq!(
        antigravity_model_slug("gemini-3.6-flash-low", Some(ReasoningEffort::High)),
        "gemini-3.6-flash-low"
    );
    assert_eq!(
        antigravity_model_slug("claude-opus-5", Some(ReasoningEffort::Low)),
        "claude-opus-5"
    );
    assert!(super::is_antigravity_reasoning_step("thinking_delta"));
    assert!(super::is_antigravity_reasoning_step("MODEL_REASONING"));
    assert!(!super::is_antigravity_reasoning_step("text_delta"));
}

#[test]
fn antigravity_usage_preserves_reasoning_and_cache_tokens() {
    let usage = antigravity_usage_to_token_usage(AntigravityUsage {
        input_tokens: 11,
        output_tokens: 13,
        thinking_tokens: 17,
        cache_read_tokens: 19,
        total_tokens: 60,
    });

    assert_eq!(usage.input_tokens, 11);
    assert_eq!(usage.cached_input_tokens, 19);
    assert_eq!(usage.output_tokens, 13);
    assert_eq!(usage.reasoning_output_tokens, 17);
    assert_eq!(usage.total_tokens, 60);
    assert_eq!(usage.provider_request_count, 0);
}

#[test]
fn clamp_usage_uses_aggregate_counters_and_last_call_context() {
    let usage = clamp_usage_to_token_usage(
        Usage {
            input_tokens: 8,
            cache_creation_input_tokens: 45_558,
            cache_read_input_tokens: 136_189,
            output_tokens: 233,
        },
        Some(&Usage {
            input_tokens: 3,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 45_700,
            output_tokens: 47,
        }),
    );

    assert_eq!(usage.input_tokens, 181_755);
    assert_eq!(usage.cache_creation_input_tokens, 45_558);
    assert_eq!(usage.cached_input_tokens, 136_189);
    assert_eq!(usage.output_tokens, 233);
    assert_eq!(usage.reasoning_output_tokens, 0);
    assert_eq!(usage.total_tokens, 45_750);
    assert_eq!(usage.provider_request_count, 0);
}

#[test]
fn clamp_usage_saturates_on_overflow() {
    let max_usage = Usage {
        input_tokens: u64::MAX,
        cache_creation_input_tokens: u64::MAX,
        cache_read_input_tokens: u64::MAX,
        output_tokens: u64::MAX,
    };
    let usage = clamp_usage_to_token_usage(max_usage.clone(), Some(&max_usage));

    assert_eq!(usage.input_tokens, i64::MAX);
    assert_eq!(usage.cache_creation_input_tokens, i64::MAX);
    assert_eq!(usage.cached_input_tokens, i64::MAX);
    assert_eq!(usage.output_tokens, i64::MAX);
    assert_eq!(usage.reasoning_output_tokens, 0);
    assert_eq!(usage.total_tokens, i64::MAX);
    assert_eq!(usage.provider_request_count, 0);
}

#[test]
fn clamp_usage_leaves_context_unknown_without_assistant_usage() {
    let usage = clamp_usage_to_token_usage(
        Usage {
            input_tokens: 11,
            cache_creation_input_tokens: 13,
            cache_read_input_tokens: 17,
            output_tokens: 19,
        },
        None,
    );

    assert_eq!(usage.input_tokens, 41);
    assert_eq!(usage.output_tokens, 19);
    assert_eq!(usage.total_tokens, 0);
}
#[test]
fn claude_failures_cannot_replay_a_native_turn_through_sampling_retries() {
    for error in [
        chaos_clamp::ClampError::Closed,
        chaos_clamp::ClampError::TurnFailed,
        chaos_clamp::ClampError::AuthenticationUnavailable,
        chaos_clamp::ClampError::Timeout("initialize".into()),
    ] {
        let error =
            crate::api_bridge::map_api_error(super::clamp_failure(&error, "clamp_runtime_failed"));
        assert!(!error.is_retryable());
    }
}
