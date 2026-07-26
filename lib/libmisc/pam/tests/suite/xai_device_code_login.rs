#![allow(clippy::unwrap_used)]

use chaos_ipc::api::AuthMode;
use chaos_kern::auth::AuthCredentialsStoreMode;
use chaos_kern::auth::load_auth_dot_json;
use chaos_pam::XaiDeviceCodeOptions;
use chaos_pam::complete_xai_device_code_login;
use chaos_pam::request_xai_device_code;
use serde_json::json;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_string_contains;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::auth_test_support::make_jwt;

struct XaiDeviceCodeHarness {
    chaos_home: TempDir,
    mock_server: MockServer,
}

impl XaiDeviceCodeHarness {
    async fn start() -> Self {
        Self {
            chaos_home: tempfile::tempdir().unwrap(),
            mock_server: MockServer::start().await,
        }
    }

    fn options(&self) -> XaiDeviceCodeOptions {
        let mut options = XaiDeviceCodeOptions::new(
            self.chaos_home.path().to_path_buf(),
            AuthCredentialsStoreMode::File,
        );
        options.issuer = self.mock_server.uri();
        options.client_id = "xai-test-client".to_string();
        options
    }

    async fn mock_discovery(&self) {
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "token_endpoint": format!("{}/oauth2/token", self.mock_server.uri()),
                "device_authorization_endpoint": format!(
                    "{}/oauth2/device/code",
                    self.mock_server.uri()
                )
            })))
            .mount(&self.mock_server)
            .await;
    }

    async fn mock_device_code(&self) {
        let verification_uri = format!("{}/oauth2/device", self.mock_server.uri());
        Mock::given(method("POST"))
            .and(path("/oauth2/device/code"))
            .and(body_string_contains("client_id=xai-test-client"))
            .and(body_string_contains("grok-cli%3Aaccess"))
            .and(body_string_contains("conversations%3Aread"))
            .and(body_string_contains("workspaces%3Awrite"))
            .and(body_string_contains("referrer=chaos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "device-secret",
                "user_code": "GROK-CODE",
                "verification_uri": verification_uri,
                "verification_uri_complete": format!(
                    "{}/oauth2/device?user_code=GROK-CODE",
                    self.mock_server.uri()
                ),
                "expires_in": 900,
                "interval": 0
            })))
            .mount(&self.mock_server)
            .await;
    }
}

#[tokio::test]
async fn xai_device_code_login_persists_provider_scoped_oauth() {
    let harness = XaiDeviceCodeHarness::start().await;
    harness.mock_discovery().await;
    harness.mock_device_code().await;
    let id_token = make_jwt(json!({
        "sub": "xai-user-123",
        "email": "grok@example.com"
    }));
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .and(body_string_contains(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
        ))
        .and(body_string_contains("device_code=device-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "xai-access-token",
            "refresh_token": "xai-refresh-token",
            "id_token": id_token
        })))
        .mount(&harness.mock_server)
        .await;

    let options = harness.options();
    let device_code = request_xai_device_code(&options).await.unwrap();
    assert_eq!(device_code.user_code, "GROK-CODE");
    assert_eq!(
        device_code.verification_url,
        format!("{}/oauth2/device", harness.mock_server.uri())
    );
    complete_xai_device_code_login(options, device_code)
        .await
        .unwrap();

    let auth = load_auth_dot_json(harness.chaos_home.path(), AuthCredentialsStoreMode::File)
        .unwrap()
        .unwrap();
    let record = auth.provider_record("xai").unwrap();
    assert_eq!(record.auth_mode, Some(AuthMode::Xai));
    assert!(record.api_key.is_none());
    let tokens = record.tokens.unwrap();
    assert_eq!(tokens.access_token, "xai-access-token");
    assert_eq!(tokens.refresh_token, "xai-refresh-token");
    assert_eq!(tokens.account_id.as_deref(), Some("xai-user-123"));
    assert_eq!(tokens.id_token.email.as_deref(), Some("grok@example.com"));
}

#[tokio::test]
async fn xai_device_code_login_uses_access_token_identity_when_id_token_is_omitted() {
    let harness = XaiDeviceCodeHarness::start().await;
    harness.mock_discovery().await;
    harness.mock_device_code().await;
    let access_token = make_jwt(json!({
        "sub": "xai-access-user",
        "email": "access-token@example.com"
    }));
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": access_token,
            "refresh_token": "xai-refresh-token"
        })))
        .mount(&harness.mock_server)
        .await;

    let options = harness.options();
    let device_code = request_xai_device_code(&options).await.unwrap();
    complete_xai_device_code_login(options, device_code)
        .await
        .unwrap();

    let auth = load_auth_dot_json(harness.chaos_home.path(), AuthCredentialsStoreMode::File)
        .unwrap()
        .unwrap();
    let tokens = auth.provider_record("xai").unwrap().tokens.unwrap();
    assert_eq!(tokens.account_id.as_deref(), Some("xai-access-user"));
    assert_eq!(
        tokens.id_token.email.as_deref(),
        Some("access-token@example.com")
    );
}

#[tokio::test]
async fn xai_device_code_login_rejects_untrusted_verification_origin() {
    let harness = XaiDeviceCodeHarness::start().await;
    harness.mock_discovery().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/device/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "device-secret",
            "user_code": "GROK-CODE",
            "verification_uri": "https://attacker.example/device",
            "expires_in": 900
        })))
        .mount(&harness.mock_server)
        .await;

    let error = request_xai_device_code(&harness.options())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unexpected origin"));
}

#[tokio::test]
async fn xai_device_code_login_does_not_persist_denied_authorization() {
    let harness = XaiDeviceCodeHarness::start().await;
    harness.mock_discovery().await;
    harness.mock_device_code().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "access_denied",
            "error_description": "The user denied the request"
        })))
        .mount(&harness.mock_server)
        .await;

    let options = harness.options();
    let device_code = request_xai_device_code(&options).await.unwrap();
    let error = complete_xai_device_code_login(options, device_code)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(
        load_auth_dot_json(harness.chaos_home.path(), AuthCredentialsStoreMode::File)
            .unwrap()
            .is_none()
    );
}
