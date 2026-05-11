//! Header-building helpers shared across HTTP adapters.
//!
//! Every adapter in this crate constructs a [`HeaderMap`] for its
//! outbound request. The shape is similar across providers but the
//! auth scheme varies: OpenAI-style endpoints use a Bearer token;
//! Anthropic accepts either a Bearer token or an `x-api-key` value;
//! TensorZero treats the bearer as optional. The helpers here cover
//! the mechanical parts — inserting `Authorization`, `x-api-key`, and
//! the standard JSON-in / SSE-out pair — and leave the policy choices
//! (mandatory vs. optional, header value source) to the caller.
//!
//! Keep these functions narrow: they produce or mutate a `HeaderMap`
//! and never reach for the network. Higher-level orchestration (retry
//! loops, SSE wiring) lives in [`crate::sse::transport`].

use crate::common::MIME_APPLICATION_JSON;
use crate::common::MIME_TEXT_EVENT_STREAM;
use chaos_abi::AbiError;
use http::HeaderMap;
use http::HeaderValue;
use http::header;

/// Insert `Authorization: Bearer <api_key>`. Returns
/// `AbiError::InvalidRequest` if the key is empty or the formatted
/// value would not be a legal HTTP header. The caller chooses whether
/// the bearer is mandatory; pass an empty key only after deciding the
/// adapter does not require one.
pub(crate) fn insert_bearer_auth(
    headers: &mut HeaderMap,
    api_key: &str,
    provider_label: &str,
) -> Result<(), AbiError> {
    if api_key.trim().is_empty() {
        return Err(AbiError::InvalidRequest {
            message: format!("{provider_label} provider requires a non-empty API key"),
        });
    }
    let bearer = format!("Bearer {api_key}");
    let value = HeaderValue::from_str(&bearer).map_err(|err| AbiError::InvalidRequest {
        message: format!("invalid Authorization header value: {err}"),
    })?;
    headers.insert(header::AUTHORIZATION, value);
    Ok(())
}

/// Insert `x-api-key: <api_key>` (Anthropic-style auth).
pub(crate) fn insert_api_key_header(
    headers: &mut HeaderMap,
    api_key: &str,
    provider_label: &str,
) -> Result<(), AbiError> {
    if api_key.trim().is_empty() {
        return Err(AbiError::InvalidRequest {
            message: format!("{provider_label} requires a non-empty API key"),
        });
    }
    let value = HeaderValue::from_str(api_key).map_err(|err| AbiError::InvalidRequest {
        message: format!("invalid x-api-key header value: {err}"),
    })?;
    headers.insert("x-api-key", value);
    Ok(())
}

/// Insert the standard `Content-Type: application/json` and
/// `Accept: text/event-stream` pair that every streaming adapter in
/// this crate sends.
pub(crate) fn insert_streaming_json_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(MIME_APPLICATION_JSON),
    );
    headers.insert(
        header::ACCEPT,
        HeaderValue::from_static(MIME_TEXT_EVENT_STREAM),
    );
}

#[cfg(test)]
mod tests {
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
}
