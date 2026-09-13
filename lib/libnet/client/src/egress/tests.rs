use super::*;
use rama::extensions::ExtensionsRef;
use rama::http::Body;

#[test]
fn routes_every_vendor_without_changing_payload_or_credentials() {
    let egress = Egress::parse("http://gateway:3000/egress/chaos/").unwrap();
    for upstream in [
        "https://api.openai.com/v1/responses",
        "https://chatgpt.com/backend-api/codex/responses",
        "https://cli-chat-proxy.grok.com/v1/responses",
        "https://api.anthropic.com/v1/messages?beta=true",
        "https://api.minimax.io/anthropic/v1/messages",
        "https://deployment.openai.azure.com/openai/responses?api-version=2025-03-01",
        "https://custom.example/v1/files/a%2Fb?key=a%2Bb",
        "http://localhost:11434/v1/chat/completions",
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri(upstream)
            .header("authorization", "Bearer vendor-token")
            .header("x-api-key", "vendor-key")
            .header("host", "stale.example")
            .header(EGRESS_UPSTREAM_HEADER, "https://spoofed.example")
            .body("unchanged bytes")
            .unwrap();
        let target = request.uri().request_target().into_owned();
        let url = Url::parse(upstream).unwrap();
        request
            .extensions()
            .insert(rama::net::client::ConnectorTarget(
                "vendor.example:443".parse().unwrap(),
            ));
        egress.route(&mut request).unwrap();
        assert!(
            !request
                .extensions()
                .contains::<rama::net::client::ConnectorTarget>()
        );
        assert_eq!(
            request.uri().to_string(),
            format!("http://gateway:3000/egress/chaos{target}")
        );
        assert_eq!(
            request.headers()[EGRESS_UPSTREAM_HEADER],
            url.origin().ascii_serialization()
        );
        assert_eq!(request.headers()["authorization"], "Bearer vendor-token");
        assert_eq!(request.headers()["x-api-key"], "vendor-key");
        assert!(!request.headers().contains_key("host"));
        assert_eq!(request.method(), "POST");
        assert_eq!(*request.body(), "unchanged bytes");
    }
}

#[test]
fn rejects_invalid_gateway_configuration() {
    for endpoint in [
        "",
        "gateway:3000/egress/chaos",
        "ftp://gateway/egress/chaos",
        "https://user:password@gateway/egress/chaos",
        "https://gateway/egress/chaos?token=secret",
        "https://gateway/egress/chaos#fragment",
    ] {
        assert!(Egress::parse(endpoint).is_err(), "{endpoint}");
    }
}

#[tokio::test]
async fn gateway_failure_is_returned_without_direct_fallback() {
    let client = rama::service::service_fn(|request: Request| async move {
        assert_eq!(
            request.uri().to_string(),
            "http://gateway/egress/chaos/v1/responses"
        );
        Err::<Response, _>(OpaqueError::from_static_str("gateway unavailable"))
    })
    .boxed();
    let client = with_egress(
        client,
        Some(Egress::parse("http://gateway/egress/chaos").unwrap()),
    );
    let request = Request::builder()
        .uri("https://api.openai.com/v1/responses")
        .body(Body::empty())
        .unwrap();
    assert!(
        client
            .serve(request)
            .await
            .unwrap_err()
            .to_string()
            .contains("gateway unavailable")
    );
}
