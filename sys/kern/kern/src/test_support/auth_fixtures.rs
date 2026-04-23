#![allow(dead_code)]

use base64::Engine;
use chaos_ipc::api::AuthMode as ApiAuthMode;
use jiff::Timestamp;
use serde_json::json;

use crate::auth::AuthDotJson;
use crate::auth::DEFAULT_AUTH_PROVIDER_ID;
use crate::auth::ProviderAuthRecord;
use crate::token_data::IdTokenInfo;
use crate::token_data::TokenData;
use crate::token_data::parse_chatgpt_jwt_claims;

pub(super) fn make_jwt(payload: serde_json::Value) -> String {
    let header = json!({ "alg": "none", "typ": "JWT" });
    let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header_b64 = encode(&serde_json::to_vec(&header).unwrap_or_else(|err| {
        panic!("header should serialize: {err}");
    }));
    let payload_b64 = encode(&serde_json::to_vec(&payload).unwrap_or_else(|err| {
        panic!("payload should serialize: {err}");
    }));
    let signature_b64 = encode(b"sig");
    format!("{header_b64}.{payload_b64}.{signature_b64}")
}

pub(super) fn build_fake_jwt(
    chatgpt_plan_type: Option<&str>,
    chatgpt_account_id: Option<&str>,
) -> String {
    let mut auth_payload = json!({
        "chatgpt_user_id": "user-12345",
        "user_id": "user-12345",
    });

    if let Some(chatgpt_plan_type) = chatgpt_plan_type {
        auth_payload["chatgpt_plan_type"] =
            serde_json::Value::String(chatgpt_plan_type.to_string());
    }

    if let Some(chatgpt_account_id) = chatgpt_account_id {
        auth_payload["chatgpt_account_id"] =
            serde_json::Value::String(chatgpt_account_id.to_string());
    }

    make_jwt(json!({
        "email": "user@example.com",
        "email_verified": true,
        "https://api.openai.com/auth": auth_payload,
    }))
}

pub(super) fn parse_id_token(raw_jwt: &str) -> IdTokenInfo {
    parse_chatgpt_jwt_claims(raw_jwt).unwrap_or_else(|err| panic!("fake JWT should parse: {err}"))
}

pub(super) fn id_token_from_payload(payload: serde_json::Value) -> IdTokenInfo {
    parse_id_token(&make_jwt(payload))
}

pub(super) fn openai_auth(
    auth_mode: ApiAuthMode,
    api_key: Option<&str>,
    tokens: Option<TokenData>,
    last_refresh: Option<Timestamp>,
) -> AuthDotJson {
    AuthDotJson {
        providers: [(
            DEFAULT_AUTH_PROVIDER_ID.to_string(),
            ProviderAuthRecord {
                auth_mode: Some(auth_mode),
                api_key: api_key.map(str::to_string),
                tokens,
                last_refresh,
            },
        )]
        .into_iter()
        .collect(),
    }
}

pub(super) fn openai_record(auth: &AuthDotJson) -> &ProviderAuthRecord {
    auth.providers
        .get(DEFAULT_AUTH_PROVIDER_ID)
        .unwrap_or_else(|| panic!("openai provider record should exist"))
}

pub(super) fn build_tokens(access_token: &str, refresh_token: &str) -> TokenData {
    let id_token = IdTokenInfo {
        raw_jwt: make_jwt(json!({ "sub": "user-123" })),
        ..IdTokenInfo::default()
    };
    TokenData {
        id_token,
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
        account_id: Some("account-id".to_string()),
    }
}
