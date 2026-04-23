#![allow(clippy::unwrap_used)]

use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chaos_kern::auth::AuthCredentialsStoreMode;
use chaos_kern::auth::DEFAULT_AUTH_PROVIDER_ID;
use chaos_kern::auth::load_auth_dot_json;
use chaos_pam::ServerOptions;
use chaos_pam::run_device_code_login;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use core_test_support::skip_if_no_network;

// ---------- Small helpers  ----------

fn make_jwt(payload: serde_json::Value) -> String {
    let header = json!({ "alg": "none", "typ": "JWT" });
    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
    let signature_b64 = URL_SAFE_NO_PAD.encode(b"sig");
    format!("{header_b64}.{payload_b64}.{signature_b64}")
}

fn openai_record(auth: &chaos_kern::auth::AuthDotJson) -> &chaos_kern::auth::ProviderAuthRecord {
    assert!(
        auth.providers.contains_key(DEFAULT_AUTH_PROVIDER_ID),
        "openai provider record should exist"
    );
    &auth.providers[DEFAULT_AUTH_PROVIDER_ID]
}

struct DeviceCodeHarness {
    chaos_home: TempDir,
    mock_server: MockServer,
}

impl DeviceCodeHarness {
    async fn start() -> Self {
        Self {
            chaos_home: tempfile::tempdir().unwrap(),
            mock_server: MockServer::start().await,
        }
    }

    async fn mock_usercode_success(&self) {
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/usercode"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_auth_id": "device-auth-123",
                "user_code": "CODE-12345",
                // NOTE: Interval is kept 0 in order to avoid waiting for the interval to pass
                "interval": "0"
            })))
            .mount(&self.mock_server)
            .await;
    }

    async fn mock_usercode_failure(&self, status: u16) {
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/usercode"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&self.mock_server)
            .await;
    }

    async fn mock_poll_token_pending_then_success(&self, first_response_status: u16) {
        let counter = Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/token"))
            .respond_with(move |_: &Request| {
                let attempt = counter.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    ResponseTemplate::new(first_response_status)
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "authorization_code": "poll-code-321",
                        "code_challenge": "code-challenge-321",
                        "code_verifier": "code-verifier-321"
                    }))
                }
            })
            .expect(2)
            .mount(&self.mock_server)
            .await;
    }

    async fn mock_poll_token_response(&self, endpoint: &str, response: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(response)
            .mount(&self.mock_server)
            .await;
    }

    async fn mock_oauth_token(&self, jwt: &str) {
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id_token": jwt,
                "access_token": "access-token-123",
                "refresh_token": "refresh-token-123"
            })))
            .mount(&self.mock_server)
            .await;
    }

    async fn mock_successful_auth_flow(&self, jwt_payload: serde_json::Value) -> String {
        self.mock_usercode_success().await;
        self.mock_poll_token_pending_then_success(404).await;

        let jwt = make_jwt(jwt_payload);
        self.mock_oauth_token(&jwt).await;
        jwt
    }

    fn server_opts(
        &self,
        cli_auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> ServerOptions {
        let mut opts = ServerOptions::new(
            self.chaos_home.path().to_path_buf(),
            "client-id".to_string(),
            None,
            cli_auth_credentials_store_mode,
        );
        opts.issuer = self.mock_server.uri();
        opts.open_browser = false;
        opts
    }
}

#[tokio::test]
async fn device_code_login_integration_succeeds() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let fixture = DeviceCodeHarness::start().await;
    let jwt = fixture
        .mock_successful_auth_flow(json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_321"
            }
        }))
        .await;

    let opts = fixture.server_opts(AuthCredentialsStoreMode::File);

    run_device_code_login(opts)
        .await
        .expect("device code login integration should succeed");

    let auth = load_auth_dot_json(fixture.chaos_home.path(), AuthCredentialsStoreMode::File)
        .context("auth.json should load after login succeeds")?
        .context("auth.json written")?;
    let tokens = openai_record(&auth)
        .tokens
        .clone()
        .expect("tokens persisted");
    assert_eq!(tokens.access_token, "access-token-123");
    assert_eq!(tokens.refresh_token, "refresh-token-123");
    assert_eq!(tokens.id_token.raw_jwt, jwt);
    assert_eq!(tokens.account_id.as_deref(), Some("acct_321"));
    Ok(())
}

#[tokio::test]
async fn device_code_login_rejects_workspace_mismatch() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let fixture = DeviceCodeHarness::start().await;
    fixture
        .mock_successful_auth_flow(json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_321",
                "organization_id": "org-actual"
            }
        }))
        .await;

    let mut opts = fixture.server_opts(AuthCredentialsStoreMode::File);
    opts.forced_chatgpt_workspace_id = Some("org-required".to_string());

    let err = run_device_code_login(opts)
        .await
        .expect_err("device code login should fail when workspace mismatches");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);

    let auth = load_auth_dot_json(fixture.chaos_home.path(), AuthCredentialsStoreMode::File)
        .context("auth.json should load after login fails")?;
    assert!(
        auth.is_none(),
        "auth.json should not be created when workspace validation fails"
    );
    Ok(())
}

#[tokio::test]
async fn device_code_login_integration_handles_usercode_http_failure() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let fixture = DeviceCodeHarness::start().await;
    fixture.mock_usercode_failure(503).await;

    let opts = fixture.server_opts(AuthCredentialsStoreMode::File);

    let err = run_device_code_login(opts)
        .await
        .expect_err("usercode HTTP failure should bubble up");
    assert!(
        err.to_string()
            .contains("device code request failed with status"),
        "unexpected error: {err:?}"
    );

    let auth = load_auth_dot_json(fixture.chaos_home.path(), AuthCredentialsStoreMode::File)
        .context("auth.json should load after login fails")?;
    assert!(
        auth.is_none(),
        "auth.json should not be created when login fails"
    );
    Ok(())
}

#[tokio::test]
async fn device_code_login_integration_persists_without_api_key_on_exchange_failure()
-> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let fixture = DeviceCodeHarness::start().await;
    let jwt = fixture.mock_successful_auth_flow(json!({})).await;

    let opts = fixture.server_opts(AuthCredentialsStoreMode::File);

    run_device_code_login(opts)
        .await
        .expect("device login should succeed without API key exchange");

    let auth = load_auth_dot_json(fixture.chaos_home.path(), AuthCredentialsStoreMode::File)
        .context("auth.json should load after login succeeds")?
        .context("auth.json written")?;
    assert!(openai_record(&auth).api_key.is_none());
    let tokens = openai_record(&auth)
        .tokens
        .clone()
        .expect("tokens persisted");
    assert_eq!(tokens.access_token, "access-token-123");
    assert_eq!(tokens.refresh_token, "refresh-token-123");
    assert_eq!(tokens.id_token.raw_jwt, jwt);
    Ok(())
}

#[tokio::test]
async fn device_code_login_integration_handles_error_payload() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let fixture = DeviceCodeHarness::start().await;
    fixture.mock_usercode_success().await;

    // // /deviceauth/token → returns error payload with status 401
    fixture
        .mock_poll_token_response(
            "/api/accounts/deviceauth/token",
            ResponseTemplate::new(401).set_body_json(json!({
                "error": "authorization_declined",
                "error_description": "Denied"
            })),
        )
        .await;

    // (WireMock will automatically 404 for other paths)
    let opts = fixture.server_opts(AuthCredentialsStoreMode::File);

    let err = run_device_code_login(opts)
        .await
        .expect_err("integration failure path should return error");

    // Accept either the specific error payload, a 400, or a 404 (since the client may return 404 if the flow is incomplete)
    assert!(
        err.to_string().contains("authorization_declined") || err.to_string().contains("401"),
        "Expected an authorization_declined / 400 / 404 error, got {err:?}"
    );

    let auth = load_auth_dot_json(fixture.chaos_home.path(), AuthCredentialsStoreMode::File)
        .context("auth.json should load after login fails")?;
    assert!(
        auth.is_none(),
        "auth.json should not be created when device auth fails"
    );
    Ok(())
}
