use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn polling_stops_when_the_device_code_expires() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "authorization_pending"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let home = tempfile::tempdir().unwrap();
    let opts = XaiDeviceCodeOptions::new(home.path().to_path_buf(), AuthCredentialsStoreMode::File);
    let device_code = XaiDeviceCode {
        verification_url: format!("{}/activate", server.uri()),
        user_code: "ABCD-EFGH".into(),
        expires_in: 2,
        device_code: "device".into(),
        interval: 1,
        token_endpoint: format!("{}/token", server.uri()),
    };
    let error = complete_xai_device_code_login(opts, device_code)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("expired"), "{error}");
}
