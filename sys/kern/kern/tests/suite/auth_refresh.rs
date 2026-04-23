use anyhow::Context;
use anyhow::Result;
use base64::Engine;
use chaos_ipc::api::AuthMode;
use chaos_kern::AuthManager;
use chaos_kern::auth::AuthCredentialsStoreMode;
use chaos_kern::auth::AuthDotJson;
use chaos_kern::auth::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR;
use chaos_kern::auth::RefreshTokenError;
use chaos_kern::auth::load_auth_dot_json;
use chaos_kern::auth::save_auth;
use chaos_kern::error::RefreshTokenFailedReason;
use chaos_kern::token_data::IdTokenInfo;
use chaos_kern::token_data::TokenData;
use core_test_support::skip_if_no_network;
use jiff::Timestamp;
use jiff::ToSpan;
use pretty_assertions::assert_eq;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

const INITIAL_ACCESS_TOKEN: &str = "initial-access-token";
const INITIAL_REFRESH_TOKEN: &str = "initial-refresh-token";

fn openai_auth(
    auth_mode: AuthMode,
    api_key: Option<&str>,
    tokens: Option<TokenData>,
    last_refresh: Option<Timestamp>,
) -> AuthDotJson {
    AuthDotJson {
        providers: [(
            "openai".to_string(),
            chaos_kern::auth::ProviderAuthRecord {
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

fn openai_record(auth: &AuthDotJson) -> &chaos_kern::auth::ProviderAuthRecord {
    assert!(
        auth.providers.contains_key("openai"),
        "openai provider record should exist"
    );
    &auth.providers["openai"]
}

#[serial_test::serial(auth_refresh)]
#[tokio::test]
async fn refresh_token_succeeds_updates_storage() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new-access-token",
            "refresh_token": "new-refresh-token"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ctx = RefreshTokenTestContext::new(&server)?;
    let initial_last_refresh = timestamp_hours_ago(24)?;
    let initial_tokens = build_tokens(INITIAL_ACCESS_TOKEN, INITIAL_REFRESH_TOKEN);
    let initial_auth = openai_auth(
        AuthMode::Chatgpt,
        None,
        Some(initial_tokens.clone()),
        Some(initial_last_refresh),
    );
    ctx.write_auth(&initial_auth)?;

    ctx.auth_manager
        .refresh_token_from_authority()
        .await
        .context("refresh should succeed")?;

    let refreshed_tokens = TokenData {
        access_token: "new-access-token".to_string(),
        refresh_token: "new-refresh-token".to_string(),
        ..initial_tokens.clone()
    };
    let stored = ctx.load_auth()?;
    let tokens = openai_record(&stored)
        .tokens
        .as_ref()
        .context("tokens should exist")?;
    assert_eq!(tokens, &refreshed_tokens);
    let refreshed_at = openai_record(&stored)
        .last_refresh
        .as_ref()
        .context("last_refresh should be recorded")?;
    assert!(
        *refreshed_at >= initial_last_refresh,
        "last_refresh should advance"
    );

    let cached_auth = ctx
        .auth_manager
        .auth()
        .await
        .context("auth should be cached")?;
    let cached = cached_auth
        .get_token_data()
        .context("token data should be cached")?;
    assert_eq!(cached, refreshed_tokens);

    server.verify().await;
    Ok(())
}

#[serial_test::serial(auth_refresh)]
#[tokio::test]
async fn refresh_token_refreshes_when_auth_is_unchanged() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new-access-token",
            "refresh_token": "new-refresh-token"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ctx = RefreshTokenTestContext::new(&server)?;
    let initial_last_refresh = timestamp_hours_ago(24)?;
    let initial_tokens = build_tokens(INITIAL_ACCESS_TOKEN, INITIAL_REFRESH_TOKEN);
    let initial_auth = openai_auth(
        AuthMode::Chatgpt,
        None,
        Some(initial_tokens.clone()),
        Some(initial_last_refresh),
    );
    ctx.write_auth(&initial_auth)?;

    ctx.auth_manager
        .refresh_token()
        .await
        .context("refresh should succeed")?;

    let refreshed_tokens = TokenData {
        access_token: "new-access-token".to_string(),
        refresh_token: "new-refresh-token".to_string(),
        ..initial_tokens.clone()
    };
    let stored = ctx.load_auth()?;
    let tokens = openai_record(&stored)
        .tokens
        .as_ref()
        .context("tokens should exist")?;
    assert_eq!(tokens, &refreshed_tokens);
    let refreshed_at = openai_record(&stored)
        .last_refresh
        .as_ref()
        .context("last_refresh should be recorded")?;
    assert!(
        *refreshed_at >= initial_last_refresh,
        "last_refresh should advance"
    );

    let cached_auth = ctx
        .auth_manager
        .auth()
        .await
        .context("auth should be cached")?;
    let cached = cached_auth
        .get_token_data()
        .context("token data should be cached")?;
    assert_eq!(cached, refreshed_tokens);

    server.verify().await;
    Ok(())
}

#[serial_test::serial(auth_refresh)]
#[tokio::test]
async fn refresh_token_skips_refresh_when_auth_changed() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    let ctx = RefreshTokenTestContext::new(&server)?;

    let initial_last_refresh = timestamp_hours_ago(24)?;
    let initial_tokens = build_tokens(INITIAL_ACCESS_TOKEN, INITIAL_REFRESH_TOKEN);
    let initial_auth = openai_auth(
        AuthMode::Chatgpt,
        None,
        Some(initial_tokens),
        Some(initial_last_refresh),
    );
    ctx.write_auth(&initial_auth)?;

    let disk_tokens = build_tokens("disk-access-token", "disk-refresh-token");
    let disk_auth = openai_auth(
        AuthMode::Chatgpt,
        None,
        Some(disk_tokens.clone()),
        Some(initial_last_refresh),
    );
    save_auth(
        ctx.chaos_home.path(),
        &disk_auth,
        AuthCredentialsStoreMode::File,
    )?;

    ctx.auth_manager
        .refresh_token()
        .await
        .context("refresh should be skipped")?;

    let stored = ctx.load_auth()?;
    assert_eq!(stored, disk_auth);

    let cached_auth = ctx
        .auth_manager
        .auth_cached()
        .context("auth should be cached")?;
    let cached_tokens = cached_auth
        .get_token_data()
        .context("token data should be cached")?;
    assert_eq!(cached_tokens, disk_tokens);

    let requests = server.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "expected no refresh token requests");

    Ok(())
}

#[serial_test::serial(auth_refresh)]
#[tokio::test]
async fn refresh_token_errors_on_account_mismatch() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "recovered-access-token",
            "refresh_token": "recovered-refresh-token"
        })))
        .expect(0)
        .mount(&server)
        .await;

    let ctx = RefreshTokenTestContext::new(&server)?;
    let initial_last_refresh = timestamp_hours_ago(24)?;
    let initial_tokens = build_tokens(INITIAL_ACCESS_TOKEN, INITIAL_REFRESH_TOKEN);
    let initial_auth = openai_auth(
        AuthMode::Chatgpt,
        None,
        Some(initial_tokens.clone()),
        Some(initial_last_refresh),
    );
    ctx.write_auth(&initial_auth)?;

    let mut disk_tokens = build_tokens("disk-access-token", "disk-refresh-token");
    disk_tokens.account_id = Some("other-account".to_string());
    let disk_auth = openai_auth(
        AuthMode::Chatgpt,
        None,
        Some(disk_tokens),
        Some(initial_last_refresh),
    );
    save_auth(
        ctx.chaos_home.path(),
        &disk_auth,
        AuthCredentialsStoreMode::File,
    )?;

    let err = ctx
        .auth_manager
        .refresh_token()
        .await
        .err()
        .context("refresh should fail due to account mismatch")?;
    assert_eq!(err.failed_reason(), Some(RefreshTokenFailedReason::Other));

    let stored = ctx.load_auth()?;
    assert_eq!(stored, disk_auth);

    let requests = server.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "expected no refresh token requests");

    let cached_after = ctx
        .auth_manager
        .auth_cached()
        .context("auth should be cached after refresh")?;
    let cached_after_tokens = cached_after
        .get_token_data()
        .context("token data should remain cached")?;
    assert_eq!(cached_after_tokens, initial_tokens);

    server.verify().await;
    Ok(())
}

#[serial_test::serial(auth_refresh)]
#[tokio::test]
async fn returns_fresh_tokens_as_is() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new-access-token",
            "refresh_token": "new-refresh-token"
        })))
        .mount(&server)
        .await;

    let ctx = RefreshTokenTestContext::new(&server)?;
    let initial_last_refresh = timestamp_hours_ago(24)?;
    let initial_tokens = build_tokens(INITIAL_ACCESS_TOKEN, INITIAL_REFRESH_TOKEN);
    let initial_auth = openai_auth(
        AuthMode::Chatgpt,
        None,
        Some(initial_tokens.clone()),
        Some(initial_last_refresh),
    );
    ctx.write_auth(&initial_auth)?;

    let cached_auth = ctx
        .auth_manager
        .auth()
        .await
        .context("auth should be cached")?;
    let cached = cached_auth
        .get_token_data()
        .context("token data should remain cached")?;
    assert_eq!(cached, initial_tokens);

    let stored = ctx.load_auth()?;
    assert_eq!(stored, initial_auth);

    let requests = server.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "expected no refresh token requests");

    Ok(())
}

#[serial_test::serial(auth_refresh)]
#[tokio::test]
async fn refreshes_token_when_last_refresh_is_stale() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new-access-token",
            "refresh_token": "new-refresh-token"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ctx = RefreshTokenTestContext::new(&server)?;
    let stale_refresh = timestamp_hours_ago(216)?;
    let initial_tokens = build_tokens(INITIAL_ACCESS_TOKEN, INITIAL_REFRESH_TOKEN);
    let initial_auth = openai_auth(
        AuthMode::Chatgpt,
        None,
        Some(initial_tokens.clone()),
        Some(stale_refresh),
    );
    ctx.write_auth(&initial_auth)?;

    let cached_auth = ctx
        .auth_manager
        .auth()
        .await
        .context("auth should be cached")?;
    let refreshed_tokens = TokenData {
        access_token: "new-access-token".to_string(),
        refresh_token: "new-refresh-token".to_string(),
        ..initial_tokens.clone()
    };
    let cached = cached_auth
        .get_token_data()
        .context("token data should refresh")?;
    assert_eq!(cached, refreshed_tokens);

    let stored = ctx.load_auth()?;
    let tokens = openai_record(&stored)
        .tokens
        .as_ref()
        .context("tokens should exist")?;
    assert_eq!(tokens, &refreshed_tokens);
    let refreshed_at = openai_record(&stored)
        .last_refresh
        .as_ref()
        .context("last_refresh should be recorded")?;
    assert!(
        *refreshed_at >= stale_refresh,
        "last_refresh should advance"
    );

    server.verify().await;
    Ok(())
}

#[serial_test::serial(auth_refresh)]
#[tokio::test]
async fn refresh_token_returns_permanent_error_for_expired_refresh_token() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {
                "code": "refresh_token_expired"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ctx = RefreshTokenTestContext::new(&server)?;
    let initial_last_refresh = timestamp_hours_ago(24)?;
    let initial_tokens = build_tokens(INITIAL_ACCESS_TOKEN, INITIAL_REFRESH_TOKEN);
    let initial_auth = openai_auth(
        AuthMode::Chatgpt,
        None,
        Some(initial_tokens.clone()),
        Some(initial_last_refresh),
    );
    ctx.write_auth(&initial_auth)?;

    let err = ctx
        .auth_manager
        .refresh_token_from_authority()
        .await
        .err()
        .context("refresh should fail")?;
    assert_eq!(err.failed_reason(), Some(RefreshTokenFailedReason::Expired));

    let stored = ctx.load_auth()?;
    assert_eq!(stored, initial_auth);
    let cached_auth = ctx
        .auth_manager
        .auth()
        .await
        .context("auth should remain cached")?;
    let cached = cached_auth
        .get_token_data()
        .context("token data should remain cached")?;
    assert_eq!(cached, initial_tokens);

    server.verify().await;
    Ok(())
}

#[serial_test::serial(auth_refresh)]
#[tokio::test]
async fn refresh_token_returns_transient_error_on_server_failure() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": "temporary-failure"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ctx = RefreshTokenTestContext::new(&server)?;
    let initial_last_refresh = timestamp_hours_ago(24)?;
    let initial_tokens = build_tokens(INITIAL_ACCESS_TOKEN, INITIAL_REFRESH_TOKEN);
    let initial_auth = openai_auth(
        AuthMode::Chatgpt,
        None,
        Some(initial_tokens.clone()),
        Some(initial_last_refresh),
    );
    ctx.write_auth(&initial_auth)?;

    let err = ctx
        .auth_manager
        .refresh_token_from_authority()
        .await
        .err()
        .context("refresh should fail")?;
    assert!(matches!(err, RefreshTokenError::Transient(_)));
    assert_eq!(err.failed_reason(), None);

    let stored = ctx.load_auth()?;
    assert_eq!(stored, initial_auth);
    let cached_auth = ctx
        .auth_manager
        .auth()
        .await
        .context("auth should remain cached")?;
    let cached = cached_auth
        .get_token_data()
        .context("token data should remain cached")?;
    assert_eq!(cached, initial_tokens);

    server.verify().await;
    Ok(())
}

#[serial_test::serial(auth_refresh)]
#[tokio::test]
async fn unauthorized_recovery_reloads_then_refreshes_tokens() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "recovered-access-token",
            "refresh_token": "recovered-refresh-token"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ctx = RefreshTokenTestContext::new(&server)?;
    let initial_last_refresh = timestamp_hours_ago(24)?;
    let initial_tokens = build_tokens(INITIAL_ACCESS_TOKEN, INITIAL_REFRESH_TOKEN);
    let initial_auth = openai_auth(
        AuthMode::Chatgpt,
        None,
        Some(initial_tokens.clone()),
        Some(initial_last_refresh),
    );
    ctx.write_auth(&initial_auth)?;

    let disk_tokens = build_tokens("disk-access-token", "disk-refresh-token");
    let disk_auth = openai_auth(
        AuthMode::Chatgpt,
        None,
        Some(disk_tokens.clone()),
        Some(initial_last_refresh),
    );
    save_auth(
        ctx.chaos_home.path(),
        &disk_auth,
        AuthCredentialsStoreMode::File,
    )?;

    let cached_before = ctx
        .auth_manager
        .auth_cached()
        .expect("auth should be cached");
    let cached_before_tokens = cached_before
        .get_token_data()
        .context("token data should be cached")?;
    assert_eq!(cached_before_tokens, initial_tokens);

    let mut recovery = ctx.auth_manager.unauthorized_recovery();
    assert!(recovery.has_next());

    recovery.next().await?;

    let cached_after = ctx
        .auth_manager
        .auth_cached()
        .expect("auth should be cached after reload");
    let cached_after_tokens = cached_after
        .get_token_data()
        .context("token data should reload")?;
    assert_eq!(cached_after_tokens, disk_tokens);

    let requests = server.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "expected no refresh token requests");

    recovery.next().await?;

    let refreshed_tokens = TokenData {
        access_token: "recovered-access-token".to_string(),
        refresh_token: "recovered-refresh-token".to_string(),
        ..disk_tokens.clone()
    };
    let stored = ctx.load_auth()?;
    let tokens = openai_record(&stored)
        .tokens
        .as_ref()
        .context("tokens should exist")?;
    assert_eq!(tokens, &refreshed_tokens);

    let cached_auth = ctx
        .auth_manager
        .auth()
        .await
        .expect("auth should be cached");
    let cached_tokens = cached_auth
        .get_token_data()
        .context("token data should be cached")?;
    assert_eq!(cached_tokens, refreshed_tokens);
    assert!(!recovery.has_next());

    server.verify().await;
    Ok(())
}

#[serial_test::serial(auth_refresh)]
#[tokio::test]
async fn unauthorized_recovery_errors_on_account_mismatch() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "recovered-access-token",
            "refresh_token": "recovered-refresh-token"
        })))
        .expect(0)
        .mount(&server)
        .await;

    let ctx = RefreshTokenTestContext::new(&server)?;
    let initial_last_refresh = timestamp_hours_ago(24)?;
    let initial_tokens = build_tokens(INITIAL_ACCESS_TOKEN, INITIAL_REFRESH_TOKEN);
    let initial_auth = openai_auth(
        AuthMode::Chatgpt,
        None,
        Some(initial_tokens.clone()),
        Some(initial_last_refresh),
    );
    ctx.write_auth(&initial_auth)?;

    let mut disk_tokens = build_tokens("disk-access-token", "disk-refresh-token");
    disk_tokens.account_id = Some("other-account".to_string());
    let disk_auth = openai_auth(
        AuthMode::Chatgpt,
        None,
        Some(disk_tokens),
        Some(initial_last_refresh),
    );
    save_auth(
        ctx.chaos_home.path(),
        &disk_auth,
        AuthCredentialsStoreMode::File,
    )?;

    let cached_before = ctx
        .auth_manager
        .auth_cached()
        .expect("auth should be cached");
    let cached_before_tokens = cached_before
        .get_token_data()
        .context("token data should be cached")?;
    assert_eq!(cached_before_tokens, initial_tokens);

    let mut recovery = ctx.auth_manager.unauthorized_recovery();
    assert!(recovery.has_next());

    let err = recovery
        .next()
        .await
        .err()
        .context("recovery should fail due to account mismatch")?;
    assert_eq!(err.failed_reason(), Some(RefreshTokenFailedReason::Other));

    let stored = ctx.load_auth()?;
    assert_eq!(stored, disk_auth);

    let requests = server.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "expected no refresh token requests");

    let cached_after = ctx
        .auth_manager
        .auth_cached()
        .context("auth should remain cached after refresh")?;
    let cached_after_tokens = cached_after
        .get_token_data()
        .context("token data should remain cached")?;
    assert_eq!(cached_after_tokens, initial_tokens);

    server.verify().await;
    Ok(())
}

#[serial_test::serial(auth_refresh)]
#[tokio::test]
async fn unauthorized_recovery_requires_chatgpt_auth() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    let ctx = RefreshTokenTestContext::new(&server)?;
    let auth = openai_auth(AuthMode::ApiKey, Some("sk-test"), None, None);
    ctx.write_auth(&auth)?;

    let mut recovery = ctx.auth_manager.unauthorized_recovery();
    assert!(!recovery.has_next());

    let err = recovery
        .next()
        .await
        .err()
        .context("recovery should fail")?;
    assert_eq!(err.failed_reason(), Some(RefreshTokenFailedReason::Other));

    let requests = server.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "expected no refresh token requests");

    Ok(())
}

struct RefreshTokenTestContext {
    chaos_home: TempDir,
    auth_manager: Arc<AuthManager>,
    _env_guard: EnvVarGuard,
}

impl RefreshTokenTestContext {
    fn new(server: &MockServer) -> Result<Self> {
        let chaos_home = TempDir::new()?;

        let endpoint = format!("{}/oauth/token", server.uri());
        let env_guard = EnvVarGuard::set(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, endpoint);

        let auth_manager = AuthManager::shared(
            chaos_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        );

        Ok(Self {
            chaos_home,
            auth_manager,
            _env_guard: env_guard,
        })
    }

    fn load_auth(&self) -> Result<AuthDotJson> {
        load_auth_dot_json(self.chaos_home.path(), AuthCredentialsStoreMode::File)
            .context("load auth.json")?
            .context("auth.json should exist")
    }

    fn write_auth(&self, auth_dot_json: &AuthDotJson) -> Result<()> {
        save_auth(
            self.chaos_home.path(),
            auth_dot_json,
            AuthCredentialsStoreMode::File,
        )?;
        self.auth_manager.reload();
        Ok(())
    }
}

use chaos_kern::test_support::EnvVarGuard;

fn timestamp_hours_ago(hours: i64) -> Result<Timestamp> {
    Timestamp::now()
        .checked_sub(hours.hours())
        .context("timestamp subtraction should succeed")
}

fn minimal_jwt() -> String {
    #[derive(Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }

    let header = Header {
        alg: "none",
        typ: "JWT",
    };
    let payload = json!({ "sub": "user-123" });

    fn b64(data: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
    }

    let header_bytes = match serde_json::to_vec(&header) {
        Ok(bytes) => bytes,
        Err(err) => panic!("serialize header: {err}"),
    };
    let payload_bytes = match serde_json::to_vec(&payload) {
        Ok(bytes) => bytes,
        Err(err) => panic!("serialize payload: {err}"),
    };
    let header_b64 = b64(&header_bytes);
    let payload_b64 = b64(&payload_bytes);
    let signature_b64 = b64(b"sig");
    format!("{header_b64}.{payload_b64}.{signature_b64}")
}

fn build_tokens(access_token: &str, refresh_token: &str) -> TokenData {
    let mut id_token = IdTokenInfo::default();
    id_token.raw_jwt = minimal_jwt();
    TokenData {
        id_token,
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
        account_id: Some("account-id".to_string()),
    }
}
