use super::super::*;
use std::num::NonZeroU32;

fn client(clamped: bool) -> crate::client::ModelClient {
    crate::client::ModelClient::new(
        None,
        chaos_ipc::ProcessId::new(),
        "test".into(),
        crate::ModelProviderInfo::create_openai_provider(None),
        chaos_ipc::protocol::SessionSource::Exec,
        chaos_ipc::protocol::ApprovalPolicy::Headless,
        None,
        false,
        None,
        clamped,
        crate::config::ClampSettings::default(),
    )
}

#[tokio::test]
async fn bounded_sampling_request_has_no_native_tools_and_preserves_ordinary_calls() {
    let (_, context) = crate::chaos::make_session_and_context().await;
    let client = client(false);
    let mut model = context.model_info;
    model.native_server_side_tools = vec!["web_search".into()];
    let prompt = Prompt {
        parallel_tool_calls: true,
        ..Default::default()
    };
    let provider = client.state.provider.to_api_provider(None).unwrap();
    let bounded = client.new_text_sampling_session(NonZeroU32::new(19).unwrap());
    let ordinary = client.new_session();
    let options = ApiResponsesOptions::default();
    let make = |session: &ModelClientSession| {
        session
            .build_http_turn_request(
                &provider,
                &prompt,
                &model,
                HttpTurnRequestConfig {
                    effort: None,
                    summary: ReasoningSummaryConfig::None,
                    service_tier: None,
                    options: &options,
                },
            )
            .unwrap()
    };
    let request = make(&bounded);
    assert_eq!(request.extensions["max_output_tokens"], 19);
    assert_eq!(request.extensions["openai_tools"], json!([]));
    assert!(request.tools.is_empty());
    assert!(!request.parallel_tool_calls);
    assert!(
        request
            .turn_state
            .as_ref()
            .is_none_or(|state| state.get().is_none())
    );
    let normal = make(&ordinary);
    assert!(!normal.extensions.contains_key("max_output_tokens"));
    assert_eq!(
        normal.extensions["openai_tools"],
        json!([{"type":"web_search"}])
    );
    assert!(normal.parallel_tool_calls);
}

#[tokio::test]
async fn bounded_sampling_refuses_clamped_before_provider_auth_or_cli_startup() {
    let (_, context) = crate::chaos::make_session_and_context().await;
    let mut session = client(true).new_text_sampling_session(NonZeroU32::new(5).unwrap());
    let error = session
        .stream(
            &Prompt::default(),
            &context.model_info,
            &context.session_telemetry,
            None,
            ReasoningSummaryConfig::None,
            None,
            None,
        )
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("unsupported on clamped"), "{error}");
}
