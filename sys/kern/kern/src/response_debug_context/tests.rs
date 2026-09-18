use super::ResponseDebugContext;
use super::extract_response_debug_context;
use super::telemetry_api_error_message;
use super::telemetry_transport_error_message;
use chaos_parrot::TransportError;
use chaos_parrot::error::ApiError;
use pretty_assertions::assert_eq;
use rama::http::HeaderMap;
use rama::http::HeaderValue;
use rama::http::StatusCode;

#[test]
fn extract_response_debug_context_decodes_identity_headers() {
    let mut headers = HeaderMap::new();
    headers.insert("x-oai-request-id", HeaderValue::from_static("req-auth"));
    headers.insert("cf-ray", HeaderValue::from_static("ray-auth"));
    headers.insert(
        "x-openai-authorization-error",
        HeaderValue::from_static("missing_authorization_header"),
    );
    headers.insert(
        "x-error-json",
        HeaderValue::from_static("eyJlcnJvciI6eyJjb2RlIjoidG9rZW5fZXhwaXJlZCJ9fQ=="),
    );

    let context = extract_response_debug_context(&TransportError::Http {
        status: StatusCode::UNAUTHORIZED,
        url: Some(chaos_services::openai::CHATGPT_MODELS_URL.to_string()),
        headers: Some(headers),
        body: Some(r#"{"error":{"message":"plain text error"},"status":401}"#.to_string()),
    });

    assert_eq!(
        context,
        ResponseDebugContext {
            request_id: Some("req-auth".to_string()),
            cf_ray: Some("ray-auth".to_string()),
            auth_error: Some("missing_authorization_header".to_string()),
            auth_error_code: Some("token_expired".to_string()),
        }
    );
}

#[test]
fn telemetry_error_messages_omit_http_bodies() {
    let transport = TransportError::Http {
        status: StatusCode::UNAUTHORIZED,
        url: Some(chaos_services::openai::CHATGPT_RESPONSES_URL.to_string()),
        headers: None,
        body: Some(r#"{"error":{"message":"secret token leaked"}}"#.to_string()),
    };

    assert_eq!(telemetry_transport_error_message(&transport), "http 401");
    assert_eq!(
        telemetry_api_error_message(&ApiError::Transport(transport)),
        "http 401"
    );
}

#[test]
fn telemetry_error_messages_preserve_non_http_details() {
    let network = TransportError::Network("dns lookup failed".to_string());
    let build = TransportError::Build("invalid header value".to_string());
    let stream = ApiError::Stream("socket closed".to_string());

    assert_eq!(
        telemetry_transport_error_message(&network),
        "dns lookup failed"
    );
    assert_eq!(
        telemetry_transport_error_message(&build),
        "invalid header value"
    );
    assert_eq!(telemetry_api_error_message(&stream), "socket closed");
    assert_eq!(
        telemetry_api_error_message(&ApiError::ServiceUnavailable),
        "service unavailable"
    );
}
