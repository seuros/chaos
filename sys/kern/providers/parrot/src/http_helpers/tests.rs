use super::*;

#[test]
fn helpers_compose_into_a_full_header_map() {
    let mut headers = HeaderMap::new();
    insert_bearer_auth(&mut headers, "sk-test", "ChatCompletions").expect("bearer");
    insert_streaming_json_headers(&mut headers);

    assert_eq!(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
        Some("Bearer sk-test"),
    );
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some(MIME_APPLICATION_JSON),
    );
    assert_eq!(
        headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()),
        Some(MIME_TEXT_EVENT_STREAM),
    );
}

#[test]
fn bearer_helper_rejects_blank_keys() {
    let mut headers = HeaderMap::new();
    let err = insert_bearer_auth(&mut headers, "   ", "ChatCompletions").unwrap_err();
    assert!(matches!(err, AbiError::InvalidRequest { .. }));
    assert!(headers.is_empty());
}

#[test]
fn x_api_key_helper_round_trips() {
    let mut headers = HeaderMap::new();
    insert_api_key_header(&mut headers, "sk-ant", "Anthropic").expect("api-key");
    assert_eq!(
        headers.get("x-api-key").and_then(|v| v.to_str().ok()),
        Some("sk-ant"),
    );
}
