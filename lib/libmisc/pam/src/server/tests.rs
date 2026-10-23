use pretty_assertions::assert_eq;

use super::TokenEndpointErrorDetail;
use super::parse_token_endpoint_error;
use super::redact_sensitive_query_value;
use super::redact_sensitive_url_parts;
use super::sanitize_url_for_logging;

#[test]
fn token_endpoint_errors_keep_useful_messages_without_losing_codes() {
    for (body, code, message, display) in [
        (
            r#"{"error":"invalid_grant","error_description":"refresh token expired"}"#,
            Some("invalid_grant"),
            Some("refresh token expired"),
            "refresh token expired",
        ),
        (
            r#"{"error":{"code":"proxy_auth_required","message":"proxy authentication required"}}"#,
            Some("proxy_auth_required"),
            Some("proxy authentication required"),
            "proxy authentication required",
        ),
        (
            r#"{"error":"temporarily_unavailable"}"#,
            Some("temporarily_unavailable"),
            None,
            "temporarily_unavailable",
        ),
        ("service unavailable", None, None, "service unavailable"),
    ] {
        assert_eq!(
            parse_token_endpoint_error(body),
            TokenEndpointErrorDetail {
                error_code: code.map(str::to_owned),
                error_message: message.map(str::to_owned),
                display_message: display.to_owned(),
            },
            "{body}"
        );
    }
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
