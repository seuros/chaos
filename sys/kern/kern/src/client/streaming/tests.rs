use super::antigravity_model_slug;
use super::antigravity_usage_to_token_usage;
use super::clamp_usage_to_token_usage;
use chaos_clamp::AntigravityUsage;
use chaos_clamp::Usage;
use chaos_ipc::openai_models::ReasoningEffort;
use rama::futures;

mod sampling;

#[tokio::test]
async fn canceling_kernel_stream_releases_provider_even_when_no_more_events_arrive() {
    use rama::futures::StreamExt;
    use tokio::sync::mpsc;

    const TEST_DEADLINE: jiff::SignedDuration = jiff::SignedDuration::from_secs(5);
    let telemetry = chaos_snitch::SessionTelemetry::new(
        chaos_ipc::ProcessId::new(),
        "test",
        "test",
        None,
        "test",
        false,
        "test",
        chaos_ipc::protocol::SessionSource::Exec,
    );
    let (provider_tx, provider_rx) = mpsc::channel(1);
    let provider = futures::stream::unfold(provider_rx, |mut rx| async move {
        rx.recv().await.map(|event| (event, rx))
    })
    .boxed();
    let mut stream = super::map_response_stream(provider, telemetry);
    provider_tx
        .send(Ok(super::ResponseEvent::Created {}))
        .await
        .unwrap();
    assert!(stream.rx_event.recv().await.unwrap().is_ok());
    drop(stream);
    tokio::time::timeout(TEST_DEADLINE.unsigned_abs(), provider_tx.closed())
        .await
        .expect("our forwarding task must drop a silent provider stream on cancellation");
}

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
    assert_eq!(usage.cache_read_input_tokens, 19);
    assert_eq!(usage.output_tokens, 13);
    assert_eq!(usage.reasoning_output_tokens, 17);
    assert_eq!(usage.total_tokens, 60);
    assert_eq!(usage.provider_request_count, 0);
}

#[test]
fn prepare_user_input_keeps_audio_bytes_off_history_and_caps_one_clip_per_turn() {
    use std::io::Write;

    use chaos_audio::AudioError;
    use chaos_ipc::models::ContentItem;
    use chaos_ipc::models::ResponseInputItem;
    use chaos_ipc::models::ResponseItem;
    use chaos_ipc::user_input::UserInput;

    let mut wav = Vec::with_capacity(44 + 320);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36u32 + 320).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&320u32.to_le_bytes());
    wav.resize(44 + 320, 0);

    let mut file = tempfile::NamedTempFile::with_suffix(".wav").unwrap();
    file.write_all(&wav).unwrap();

    let prepared = crate::prompt_images::prepare_user_input_for_turn(vec![
        UserInput::Text {
            text: "Which tone came first?".to_string(),
            text_elements: Vec::new(),
        },
        UserInput::LocalAudio {
            path: file.path().to_path_buf(),
        },
    ])
    .unwrap();
    assert_eq!(prepared.audio_attachments.len(), 1);
    let attachment = &prepared.audio_attachments[0];
    assert_eq!(attachment.mime_type, "audio/wav");

    // History/rollout item must carry ONLY the permanent text note, never base64 audio bytes.
    let ResponseInputItem::Message { content, .. } = &prepared.item else {
        panic!("expected user message");
    };
    assert!(
        content
            .iter()
            .all(|item| matches!(item, ContentItem::InputText { .. }))
    );
    let history_json = serde_json::to_string(&ResponseItem::from(prepared.item.clone())).unwrap();
    assert!(history_json.contains(&format!("id {}", attachment.nonce)));
    assert!(!history_json.contains(attachment.data_base64.as_ref()));

    // Even if Turn 1 fails before any assistant response (and even after resume),
    // a subsequent plain-text turn carries zero armed audio attachments.
    let turn2 = crate::prompt_images::prepare_user_input_for_turn(vec![UserInput::Text {
        text: "Next question without audio".to_string(),
        text_elements: Vec::new(),
    }])
    .unwrap();
    assert!(turn2.audio_attachments.is_empty());

    // At most one local audio clip may be attached per turn.
    let err = crate::prompt_images::prepare_user_input_for_turn(vec![
        UserInput::LocalAudio {
            path: file.path().to_path_buf(),
        },
        UserInput::LocalAudio {
            path: file.path().to_path_buf(),
        },
    ])
    .err()
    .expect("second clip should be rejected");
    assert!(matches!(err, AudioError::TooManyAttachments));
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
    assert_eq!(usage.cache_read_input_tokens, 136_189);
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
    assert_eq!(usage.cache_read_input_tokens, i64::MAX);
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
        chaos_clamp::ClampError::TurnFailed {
            subtype: None,
            detail: None,
        },
        chaos_clamp::ClampError::AuthenticationUnavailable,
        chaos_clamp::ClampError::Timeout("initialize".into()),
    ] {
        let error =
            crate::api_bridge::map_api_error(super::clamp_failure(&error, "clamp_runtime_failed"));
        assert!(!error.is_retryable());
    }
}

#[test]
fn claude_failed_results_say_why_in_the_runtime_failure() {
    let error = chaos_clamp::ClampError::TurnFailed {
        subtype: Some("success".into()),
        detail: Some(
            "Failed to authenticate. API Error: 403 Attention Required! | Cloudflare".into(),
        ),
    };
    let chaos_parrot::error::ApiError::InvalidRequest { message } =
        super::clamp_failure(&error, "clamp_runtime_failed")
    else {
        panic!("clamp failures are invalid requests");
    };
    assert_eq!(
        message,
        "clamp_runtime_failed: Claude Code reported a failed result: \
         Failed to authenticate. API Error: 403 Attention Required! | Cloudflare"
    );
}

#[test]
fn antigravity_failures_cannot_replay_a_native_turn_through_sampling_retries() {
    for error in [
        chaos_clamp::AntigravityError::Timeout,
        chaos_clamp::AntigravityError::AuthenticationUnavailable,
        chaos_clamp::AntigravityError::Protocol("failed after tool execution".into()),
    ] {
        let error = crate::api_bridge::map_api_error(super::antigravity_failure(
            &error,
            "antigravity_runtime_failed",
        ));
        assert!(!error.is_retryable());
    }
}
