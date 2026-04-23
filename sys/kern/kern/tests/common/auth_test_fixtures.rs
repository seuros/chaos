use base64::Engine;
use chaos_ipc::api::AuthMode;
use chaos_kern::auth::AuthDotJson;
use chaos_kern::auth::DEFAULT_AUTH_PROVIDER_ID;
use chaos_kern::auth::ProviderAuthRecord;
use chaos_kern::token_data::IdTokenInfo;
use chaos_kern::token_data::TokenData;
use jiff::Timestamp;
use serde_json::json;

pub(crate) fn make_jwt(payload: serde_json::Value) -> String {
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

pub(crate) fn openai_auth(
    auth_mode: AuthMode,
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

pub(crate) fn openai_record(auth: &AuthDotJson) -> &ProviderAuthRecord {
    auth.providers
        .get(DEFAULT_AUTH_PROVIDER_ID)
        .unwrap_or_else(|| panic!("openai provider record should exist"))
}

#[allow(clippy::field_reassign_with_default)]
pub(crate) fn build_tokens(access_token: &str, refresh_token: &str) -> TokenData {
    let mut id_token = IdTokenInfo::default();
    id_token.raw_jwt = make_jwt(json!({ "sub": "user-123" }));
    TokenData {
        id_token,
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
        account_id: Some("account-id".to_string()),
    }
}
