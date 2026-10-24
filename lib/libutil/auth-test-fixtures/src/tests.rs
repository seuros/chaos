use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chaos_ipc::api::AuthMode;
use chaos_kern::auth::AuthDotJson;
use chaos_kern::auth::DEFAULT_AUTH_PROVIDER_ID;
use chaos_kern::token_data::IdTokenInfo;
use chaos_kern::token_data::TokenData;
use jiff::Timestamp;
use serde_json::json;

use super::*;

#[test]
fn jwt_keeps_unsigned_header_payload_and_placeholder_signature() {
    for payload in [json!(null), json!({"sub": "user-123", "name": "测试"})] {
        let jwt = make_jwt(payload.clone());
        let segments: Vec<_> = jwt.split('.').collect();
        assert_eq!(segments.len(), 3);
        assert!(!jwt.contains('='));
        let decode = |segment: &str| -> serde_json::Value {
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(segment).expect("base64url"))
                .expect("JSON")
        };
        assert_eq!(decode(segments[0]), json!({"alg": "none", "typ": "JWT"}));
        assert_eq!(decode(segments[1]), payload);
        assert_eq!(segments[2], "c2ln");
    }
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn tokens_keep_canned_claims_and_default_parsed_fields() {
    let tokens: TokenData = build_tokens("access", "refresh");
    let mut expected_id_token = IdTokenInfo::default();
    expected_id_token.raw_jwt = make_jwt(json!({"sub": "user-123"}));
    assert_eq!(tokens.id_token, expected_id_token);
    assert_eq!(tokens.access_token, "access");
    assert_eq!(tokens.refresh_token, "refresh");
    assert_eq!(tokens.account_id.as_deref(), Some("account-id"));
}

#[test]
fn openai_record_preserves_optional_credentials_and_refresh() {
    let tokens = build_tokens("access", "refresh");
    let refreshed: Timestamp = "2026-01-02T03:04:05Z".parse().expect("timestamp");
    let auth: AuthDotJson = openai_auth(
        AuthMode::Chatgpt,
        Some("api-key"),
        Some(tokens.clone()),
        Some(refreshed),
    );
    assert_eq!(auth.providers.len(), 1);
    let record = openai_record(&auth);
    assert!(std::ptr::eq(
        record,
        &auth.providers[DEFAULT_AUTH_PROVIDER_ID]
    ));
    assert_eq!(
        record.credential_subject.as_deref(),
        Some("test-credential-subject")
    );
    assert_eq!(record.auth_mode, Some(AuthMode::Chatgpt));
    assert_eq!(record.api_key.as_deref(), Some("api-key"));
    assert_eq!(record.tokens.as_ref(), Some(&tokens));
    assert_eq!(record.last_refresh, Some(refreshed));

    let auth = openai_auth(AuthMode::ApiKey, None, None, None);
    let record = openai_record(&auth);
    assert_eq!(record.auth_mode, Some(AuthMode::ApiKey));
    assert_eq!(record.api_key, None);
    assert_eq!(record.tokens, None);
    assert_eq!(record.last_refresh, None);
}

#[test]
#[should_panic(expected = "openai provider record should exist")]
fn missing_openai_record_panics() {
    openai_record(&AuthDotJson {
        providers: Default::default(),
    });
}
