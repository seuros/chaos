use super::AuthRequestTelemetryContext;
use super::ModelClient;
use super::PendingUnauthorizedRetry;
use super::UnauthorizedRecoveryExecution;
use super::clamp_permission_mode;
use super::render_clamp_full_prompt;
use super::render_latest_clamp_user_message;
use chaos_ipc::ProcessId;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::FunctionCallOutputPayload;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::protocol::AskForApproval;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::SubAgentSource;
use chaos_parrot::anthropic::AnthropicAuth;
use chaos_syslog::SessionTelemetry;
use pretty_assertions::assert_eq;
use serde_json::json;

fn test_model_client(session_source: SessionSource) -> ModelClient {
    let provider = crate::model_provider_info::create_oss_provider_with_base_url(
        "https://example.com/v1",
        crate::model_provider_info::WireApi::Responses,
    );
    ModelClient::new(
        None,
        ProcessId::new(),
        provider,
        session_source,
        AskForApproval::OnRequest,
        None,
        false,
        false,
        false,
        None,
    )
}

fn test_model_info() -> ModelInfo {
    serde_json::from_value(json!({
        "slug": "gpt-test",
        "display_name": "gpt-test",
        "description": "desc",
        "default_reasoning_level": "medium",
        "supported_reasoning_levels": [
            {"effort": "medium", "description": "medium"}
        ],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 1,
        "upgrade": null,
        "base_instructions": "base instructions",
        "model_messages": null,
        "supports_reasoning_summaries": false,
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {"mode": "bytes", "limit": 10000},
        "supports_parallel_tool_calls": false,
        "supports_image_detail_original": false,
        "context_window": 272000,
        "auto_compact_token_limit": null,
        "experimental_supported_tools": []
    }))
    .expect("deserialize test model info")
}

fn test_session_telemetry() -> SessionTelemetry {
    SessionTelemetry::new(
        ProcessId::new(),
        "gpt-test",
        "gpt-test",
        None,
        None,
        None,
        "test-originator".to_string(),
        false,
        "test-terminal".to_string(),
        SessionSource::Cli,
    )
}

fn test_anthropic_provider() -> crate::model_provider_info::ModelProviderInfo {
    crate::model_provider_info::ModelProviderInfo {
        name: "anthropic".into(),
        base_url: Some("https://api.anthropic.com/v1".into()),
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        wire_api: crate::model_provider_info::WireApi::Responses,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
    }
}

#[test]
fn build_subagent_headers_sets_other_subagent_label() {
    let client = test_model_client(SessionSource::SubAgent(SubAgentSource::Other(
        "memory_consolidation".to_string(),
    )));
    let headers = client.build_subagent_headers();
    let value = headers
        .get("x-openai-subagent")
        .and_then(|value| value.to_str().ok());
    assert_eq!(value, Some("memory_consolidation"));
}

#[tokio::test]
async fn summarize_memories_returns_empty_for_empty_input() {
    let client = test_model_client(SessionSource::Cli);
    let model_info = test_model_info();
    let session_telemetry = test_session_telemetry();

    let output = client
        .summarize_memories(Vec::new(), &model_info, None, &session_telemetry)
        .await
        .expect("empty summarize request should succeed");
    assert_eq!(output.len(), 0);
}

#[test]
fn auth_request_telemetry_context_tracks_attached_auth_and_retry_phase() {
    let auth_context = AuthRequestTelemetryContext::new(
        Some(crate::auth::AuthMode::Chatgpt),
        &crate::api_bridge::CoreAuthProvider::for_test(Some("access-token"), Some("workspace-123")),
        PendingUnauthorizedRetry::from_recovery(UnauthorizedRecoveryExecution {
            mode: "managed",
            phase: "refresh_token",
        }),
    );

    assert_eq!(auth_context.auth_mode, Some("Chatgpt"));
    assert!(auth_context.auth_header_attached);
    assert_eq!(auth_context.auth_header_name, Some("authorization"));
    assert!(auth_context.retry_after_unauthorized);
    assert_eq!(auth_context.recovery_mode, Some("managed"));
    assert_eq!(auth_context.recovery_phase, Some("refresh_token"));
}

#[test]
fn resolve_anthropic_auth_uses_bearer_token_from_provider_config() {
    let mut provider = test_anthropic_provider();
    provider.experimental_bearer_token = Some("anthropic-bearer".to_string());
    let client = ModelClient::new(
        None,
        ProcessId::new(),
        provider,
        SessionSource::Cli,
        AskForApproval::OnRequest,
        None,
        false,
        false,
        false,
        None,
    );
    let session = client.new_session();

    let auth = session
        .resolve_anthropic_auth()
        .expect("bearer token should resolve");

    assert_eq!(
        auth,
        AnthropicAuth::BearerToken("anthropic-bearer".to_string())
    );
}

#[test]
fn resolve_anthropic_auth_errors_when_provider_has_no_static_auth() {
    let client = ModelClient::new(
        None,
        ProcessId::new(),
        test_anthropic_provider(),
        SessionSource::Cli,
        AskForApproval::OnRequest,
        None,
        false,
        false,
        false,
        None,
    );
    let session = client.new_session();

    let err = session
        .resolve_anthropic_auth()
        .expect_err("missing auth should fail locally");

    assert!(matches!(err, crate::error::CodexErr::InvalidRequest(_)));
}

#[test]
fn clamp_permission_mode_matches_codex_session_start_mapping() {
    assert_eq!(
        clamp_permission_mode(AskForApproval::Never),
        "bypassPermissions"
    );
    assert_eq!(clamp_permission_mode(AskForApproval::OnRequest), "default");
    assert_eq!(clamp_permission_mode(AskForApproval::OnFailure), "default");
}

#[test]
fn render_clamp_full_prompt_preserves_prior_messages_and_tool_outputs() {
    let prompt = crate::client_common::Prompt {
        input: vec![
            ResponseItem::Message {
                id: None,
                role: "user".into(),
                content: vec![
                    ContentItem::InputText {
                        text: "look at this".into(),
                    },
                    ContentItem::InputImage {
                        image_url: "data:image/png;base64,AAAA".into(),
                    },
                ],
                end_turn: None,
                phase: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "shell".into(),
                namespace: None,
                arguments: "{\"command\":[\"pwd\"]}".into(),
                call_id: "call_123".into(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "call_123".into(),
                output: FunctionCallOutputPayload::from_text("/workspace\n".into()),
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText {
                    text: "I checked it.".into(),
                }],
                end_turn: Some(true),
                phase: None,
            },
        ],
        ..Default::default()
    };

    let rendered = render_clamp_full_prompt(&prompt);

    assert!(rendered.contains("look at this"));
    assert!(rendered.contains("[image: inline data omitted]"));
    assert!(rendered.contains("<function_call name=\"shell\""));
    assert!(rendered.contains("/workspace"));
    assert!(rendered.contains("I checked it."));
}

#[test]
fn render_latest_clamp_user_message_keeps_non_text_content() {
    let prompt = crate::client_common::Prompt {
        input: vec![
            ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText {
                    text: "Earlier answer".into(),
                }],
                end_turn: Some(true),
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "user".into(),
                content: vec![
                    ContentItem::InputText {
                        text: "latest prompt".into(),
                    },
                    ContentItem::InputImage {
                        image_url: "https://example.com/cat.png".into(),
                    },
                ],
                end_turn: None,
                phase: None,
            },
        ],
        ..Default::default()
    };

    let rendered = render_latest_clamp_user_message(&prompt);

    assert!(rendered.contains("latest prompt"));
    assert!(rendered.contains("[image: https://example.com/cat.png]"));
    assert!(!rendered.contains("Earlier answer"));
}
