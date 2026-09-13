use pretty_assertions::assert_eq;

use super::TokenEndpointErrorDetail;
use super::parse_token_endpoint_error;
use super::redact_sensitive_query_value;
use super::redact_sensitive_url_parts;
use super::sanitize_url_for_logging;

#[test]
fn parse_token_endpoint_error_prefers_error_description() {
    let detail = parse_token_endpoint_error(
        r#"{"error":"invalid_grant","error_description":"refresh token expired"}"#,
    );

    assert_eq!(
        detail,
        TokenEndpointErrorDetail {
            error_code: Some("invalid_grant".to_string()),
            error_message: Some("refresh token expired".to_string()),
            display_message: "refresh token expired".to_string(),
        }
    );
}

#[test]
fn parse_token_endpoint_error_reads_nested_error_message_and_code() {
    let detail = parse_token_endpoint_error(
        r#"{"error":{"code":"proxy_auth_required","message":"proxy authentication required"}}"#,
    );

    assert_eq!(
        detail,
        TokenEndpointErrorDetail {
            error_code: Some("proxy_auth_required".to_string()),
            error_message: Some("proxy authentication required".to_string()),
            display_message: "proxy authentication required".to_string(),
        }
    );
}

#[test]
fn parse_token_endpoint_error_falls_back_to_error_code() {
    let detail = parse_token_endpoint_error(r#"{"error":"temporarily_unavailable"}"#);

    assert_eq!(
        detail,
        TokenEndpointErrorDetail {
            error_code: Some("temporarily_unavailable".to_string()),
            error_message: None,
            display_message: "temporarily_unavailable".to_string(),
        }
    );
}

#[test]
fn parse_token_endpoint_error_preserves_plain_text_for_display() {
    let detail = parse_token_endpoint_error("service unavailable");

    assert_eq!(
        detail,
        TokenEndpointErrorDetail {
            error_code: None,
            error_message: None,
            display_message: "service unavailable".to_string(),
        }
    );
}

#[test]
fn redact_sensitive_query_value_only_scrubs_known_keys() {
    assert_eq!(
        redact_sensitive_query_value("code", "abc123"),
        "<redacted>".to_string()
    );
    assert_eq!(
        redact_sensitive_query_value("redirect_uri", "http://localhost:1455/auth/callback"),
        "http://localhost:1455/auth/callback".to_string()
    );
}

#[test]
fn redact_sensitive_url_parts_preserves_safe_url_shape() {
    let mut url = url::Url::parse(
        "https://user:pass@auth.openai.com/oauth/token?code=abc123&redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback#frag",
    )
    .expect("valid url");

    redact_sensitive_url_parts(&mut url);

    assert_eq!(
        url.as_str(),
        "https://auth.openai.com/oauth/token?code=%3Credacted%3E&redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"
    );
}

#[test]
fn sanitize_url_for_logging_redacts_sensitive_issuer_parts() {
    let redacted =
        sanitize_url_for_logging("https://user:pass@example.com/base?token=abc123&env=prod");

    assert_eq!(
        redacted,
        "https://example.com/base?token=%3Credacted%3E&env=prod".to_string()
    );
}
