//! Kernel transport selection, exercised through actual requests to a local peer.
use chaos_ipc::ProcessId;
use chaos_ipc::protocol::{ApprovalPolicy, SessionSource};
use chaos_kern::{ModelClient, Prompt, ResponseEvent, WireApi};
use chaos_snitch::SessionTelemetry;
use core_test_support::{load_default_config_for_test, responses};
use jiff::SignedDuration;
use rama::futures::StreamExt;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_DEADLINE: SignedDuration = SignedDuration::from_secs(10);

#[tokio::test]
async fn openai_defaults_to_v2_without_downgrade_and_explicit_disable_uses_http() {
    for (wire, disable_websocket) in [
        (WireApi::Responses, false),
        (WireApi::Auto, false),
        (WireApi::Responses, true),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/responses"))
            .and(header("upgrade", "websocket"))
            .and(header("openai-beta", "responses_websockets=2026-02-06"))
            .respond_with(ResponseTemplate::new(404))
            .expect(u64::from(!disable_websocket))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                responses::sse(vec![
                    responses::ev_response_created("http-response"),
                    responses::ev_completed("http-response"),
                ]),
                "text/event-stream",
            ))
            .expect(u64::from(disable_websocket))
            .mount(&server)
            .await;

        let home = TempDir::new().unwrap();
        let mut config = load_default_config_for_test(&home).await;
        let mut provider = chaos_kern::built_in_model_providers()["openai"].clone();
        provider.base_url = Some(format!("{}/v1", server.uri()));
        provider.experimental_bearer_token = Some("test-token".into());
        provider.wire_api = wire;
        provider.request_max_retries = Some(0);
        if disable_websocket {
            provider.supports_websockets = false;
        }
        config.model_provider = provider.clone();
        let model = chaos_kern::test_support::get_model_offline(config.model.as_deref());
        let model_info = chaos_kern::test_support::construct_model_info_offline(&model, &config);
        let process_id = ProcessId::new();
        let telemetry = SessionTelemetry::new(
            process_id,
            &model,
            &model_info.slug,
            None,
            "test",
            false,
            "test",
            SessionSource::Exec,
        );
        let client = ModelClient::new(
            None,
            process_id,
            "openai".into(),
            provider,
            SessionSource::Exec,
            ApprovalPolicy::Headless,
            None,
            false,
            None,
            false,
            Default::default(),
        );
        let mut session = client.new_session();
        let result = tokio::time::timeout(
            TEST_DEADLINE.unsigned_abs(),
            session.stream(
                &Prompt::default(),
                &model_info,
                &telemetry,
                None,
                model_info.default_reasoning_summary,
                None,
                None,
            ),
        )
        .await
        .expect("transport selection must finish");
        if disable_websocket {
            let mut stream = result.expect("explicitly disabled WS uses HTTP");
            let mut completed = false;
            while let Some(event) = stream.next().await {
                completed |= matches!(
                    event.expect("successful HTTP response"),
                    ResponseEvent::Completed { .. }
                );
            }
            assert!(completed);
        } else {
            assert!(
                matches!(result, Err(chaos_kern::error::ChaosErr::UnexpectedStatus(ref e)) if e.status.as_u16() == 404),
                "a rejected v2 upgrade must remain an error"
            );
        }
        server.verify().await;
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "no downgrade or speculative HTTP generation"
        );
    }
}
