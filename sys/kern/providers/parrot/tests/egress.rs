use chaos_abi::{AbiError, ModelAdapter, SpoolBackend, TurnRequest};
use chaos_client::{ChaosHttpClient, Egress, RamaTransport};
use chaos_parrot::Provider;
use chaos_parrot::anthropic::{AnthropicAdapter, AnthropicAuth};
use chaos_parrot::chat_completions::ChatCompletionsAdapter;
use chaos_parrot::endpoint::batches::{AnthropicSpoolBackend, XaiSpoolBackend};
use chaos_parrot::lsd::LsdAdapter;
use chaos_parrot::openai::{OpenAiAdapter, StaticAuthProvider};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn turn() -> TurnRequest {
    TurnRequest {
        model: "test-model".into(),
        instructions: "Be helpful.".into(),
        input: vec![],
        tools: vec![],
        parallel_tool_calls: true,
        reasoning: None,
        output_schema: None,
        verbosity: None,
        turn_state: None,
        extensions: Default::default(),
    }
}

#[expect(clippy::unwrap_used, reason = "wiremock supplies a valid gateway URL")]
fn provider(base_url: &str, gateway: &MockServer) -> Provider {
    let mut provider =
        Provider::from_base_url_with_default_streaming_config("Test", base_url.into(), false);
    provider.egress = Some(Egress::parse(&format!("{}/egress/chaos", gateway.uri())).unwrap());
    provider.retry.max_attempts = 1;
    provider
}

fn openai(provider: Provider) -> OpenAiAdapter<StaticAuthProvider> {
    OpenAiAdapter::new(
        RamaTransport::default_client_with_egress(provider.egress.clone()),
        provider,
        StaticAuthProvider::new(Some("vendor-token".into()), None),
        None,
        chaos_parrot::SessionRepresenter::wannabe(),
    )
}

#[tokio::test]
async fn egress_preserves_streams_and_gateway_errors_without_contacting_vendor() {
    use chaos_client::{HttpTransport, Request, TransportError};
    use futures::StreamExt;
    use rama::http::Method;

    let vendor = MockServer::start().await;
    let gateway = MockServer::start().await;
    let transport = RamaTransport::default_client_with_egress(Some(
        Egress::parse(&format!("{}/egress/chaos", gateway.uri())).unwrap(),
    ));
    let sse = "data: {\"text\":\"hello\"}\n\ndata: [DONE]\n\n";
    Mock::given(path("/egress/chaos/v1/responses"))
        .and(header("x-lsd-upstream", vendor.uri().as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-request-id", "upstream-request")
                .set_body_raw(sse, "text/event-stream"),
        )
        .expect(1)
        .mount(&gateway)
        .await;
    let request = Request::new(Method::POST, format!("{}/v1/responses", vendor.uri()));
    let mut response = transport.stream(request.clone()).await.unwrap();
    assert_eq!(response.headers["x-request-id"], "upstream-request");
    let mut bytes = Vec::new();
    while let Some(chunk) = response.bytes.next().await {
        bytes.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(bytes, sse.as_bytes());
    gateway.verify().await;
    gateway.reset().await;

    Mock::given(path("/egress/chaos/v1/responses"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("retry-after", "2")
                .set_body_string("gateway unavailable"),
        )
        .expect(1)
        .mount(&gateway)
        .await;
    let error = transport.execute(request).await.unwrap_err();
    let TransportError::Http {
        status,
        headers,
        body,
        ..
    } = error
    else {
        panic!("expected gateway status");
    };
    assert_eq!(status.as_u16(), 503);
    assert_eq!(headers.unwrap()["retry-after"], "2");
    assert_eq!(body.as_deref(), Some("gateway unavailable"));
    assert!(vendor.received_requests().await.unwrap().is_empty());
    gateway.verify().await;
}

#[tokio::test]
async fn every_wire_format_sends_vendor_authenticated_requests_through_egress() {
    let gateway = MockServer::start().await;
    let cases: Vec<(Box<dyn ModelAdapter>, &str, &str, &str)> = vec![
        (
            Box::new(openai(provider("https://api.openai.com/v1", &gateway))),
            "https://api.openai.com",
            "/v1/responses",
            "authorization",
        ),
        (
            Box::new(openai(provider(
                "https://chatgpt.com/backend-api/codex",
                &gateway,
            ))),
            "https://chatgpt.com",
            "/backend-api/codex/responses",
            "authorization",
        ),
        (
            Box::new(openai(provider(
                "https://cli-chat-proxy.grok.com/v1",
                &gateway,
            ))),
            "https://cli-chat-proxy.grok.com",
            "/v1/responses",
            "authorization",
        ),
        (
            Box::new(AnthropicAdapter::new(
                provider("https://api.anthropic.com/v1", &gateway),
                AnthropicAuth::ApiKey("vendor-token".into()),
                None,
            )),
            "https://api.anthropic.com",
            "/v1/messages",
            "x-api-key",
        ),
        (
            Box::new(ChatCompletionsAdapter::new(
                provider("https://custom.example/v1", &gateway),
                "vendor-token".into(),
                None,
            )),
            "https://custom.example",
            "/v1/chat/completions",
            "authorization",
        ),
        (
            Box::new(LsdAdapter::new(
                provider("https://inference.example", &gateway),
                "vendor-token".into(),
                None,
            )),
            "https://inference.example",
            "/inference",
            "authorization",
        ),
    ];
    for (adapter, upstream, endpoint, auth_header) in cases {
        gateway.reset().await;
        let credential = if auth_header == "authorization" {
            "Bearer vendor-token"
        } else {
            "vendor-token"
        };
        Mock::given(method("POST"))
            .and(path(format!("/egress/chaos{endpoint}")))
            .and(header("x-lsd-upstream", upstream))
            .and(header(auth_header, credential))
            .respond_with(
                ResponseTemplate::new(402).set_body_json(json!({"error": "balance exhausted"})),
            )
            .expect(1)
            .mount(&gateway)
            .await;
        let error = match adapter.stream(turn()).await {
            Err(error) => error,
            Ok(mut stream) => loop {
                match stream.rx_event.recv().await.expect("an error event") {
                    Ok(_) => continue,
                    Err(error) => break error,
                }
            },
        };
        assert!(
            matches!(error, AbiError::Transport { status: 402, .. }),
            "{upstream}: {error:?}"
        );
        gateway.verify().await;
        let requests = gateway.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "{upstream}");
        assert!(!requests[0].headers.contains_key("content-encoding"));
    }
}

#[tokio::test]
async fn model_discovery_uses_the_same_egress() {
    let gateway = MockServer::start().await;
    for (adapter, upstream) in [
        (
            Box::new(openai(provider(
                "https://cli-chat-proxy.grok.com/v1",
                &gateway,
            ))) as Box<dyn ModelAdapter>,
            "https://cli-chat-proxy.grok.com",
        ),
        (
            Box::new(AnthropicAdapter::new(
                provider("https://api.anthropic.com/v1", &gateway),
                AnthropicAuth::ApiKey("vendor-token".into()),
                None,
            )),
            "https://api.anthropic.com",
        ),
    ] {
        Mock::given(method("GET"))
            .and(path("/egress/chaos/v1/models"))
            .and(header("x-lsd-upstream", upstream))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"id": "test-model", "display_name": "Test"}]
            })))
            .expect(1)
            .mount(&gateway)
            .await;
        assert_eq!(adapter.list_models().await.unwrap()[0].id, "test-model");
    }
    gateway.verify().await;
}

#[tokio::test]
async fn batch_requests_use_the_same_egress() {
    let gateway = MockServer::start().await;
    let client = ChaosHttpClient::default_client_with_egress(Some(
        Egress::parse(&format!("{}/egress/chaos", gateway.uri())).unwrap(),
    ));
    for (backend, upstream, endpoint) in [
        (
            Box::new(
                AnthropicSpoolBackend::new("vendor-token".into(), "test".into())
                    .with_http_client(client.clone()),
            ) as Box<dyn SpoolBackend>,
            "https://api.anthropic.com",
            "/v1/messages/batches/batch-id",
        ),
        (
            Box::new(
                XaiSpoolBackend::new("vendor-token".into(), "test".into()).with_http_client(client),
            ),
            "https://api.x.ai",
            "/v1/batches/batch-id",
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/egress/chaos{endpoint}")))
            .and(header("x-lsd-upstream", upstream))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&gateway)
            .await;
        assert!(matches!(
            backend.poll("batch-id").await,
            Err(chaos_abi::SpoolError::Auth)
        ));
    }
    gateway.verify().await;
}
