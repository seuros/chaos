#![allow(clippy::unwrap_used)]
use std::io;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use super::auth_test_support::build_tokens;
use super::auth_test_support::make_jwt;
use super::auth_test_support::openai_auth;
use super::auth_test_support::openai_record;
use anyhow::Result;
use chaos_ipc::api::AuthMode;
use chaos_kern::auth::AuthCredentialsStoreMode;
use chaos_kern::auth::load_auth_dot_json;
use chaos_kern::auth::save_auth;
use chaos_pam::ServerOptions;
use chaos_pam::run_login_server;
use codex_client::ChaosHttpClient;
use codex_client::ChaosResponse;
use core_test_support::skip_if_no_network;
use tempfile::TempDir;

/// GET `url`, following a single HTTP redirect if the server returns 3xx.
/// The Rama `EasyHttpWebClient` does not auto-follow redirects, so the test
/// helper handles it explicitly.
async fn get_following_redirect(client: &ChaosHttpClient, url: &str) -> Result<ChaosResponse> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if resp.status().is_redirection() {
        let location = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("redirect missing Location header"))?
            .to_string();
        Ok(client
            .get(&location)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?)
    } else {
        Ok(resp)
    }
}

fn issuer_url(addr: SocketAddr) -> String {
    format!("http://{}:{}", addr.ip(), addr.port())
}

fn start_mock_issuer(chatgpt_account_id: &str) -> (SocketAddr, thread::JoinHandle<()>) {
    // Bind to a random available port
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tiny_http::Server::from_listener(listener, None).unwrap();
    let chatgpt_account_id = chatgpt_account_id.to_string();

    let handle = thread::spawn(move || {
        while let Ok(mut req) = server.recv() {
            let url = req.url().to_string();
            if url.starts_with("/oauth/token") {
                // Read body
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                // Build minimal JWT with plan=pro
                let id_token = make_jwt(serde_json::json!({
                    "email": "user@example.com",
                    "https://api.openai.com/auth": {
                        "chatgpt_plan_type": "pro",
                        "chatgpt_account_id": chatgpt_account_id,
                    }
                }));

                let tokens = serde_json::json!({
                    "id_token": id_token,
                    "access_token": "access-123",
                    "refresh_token": "refresh-123",
                });
                let data = serde_json::to_vec(&tokens).unwrap();
                let mut resp = tiny_http::Response::from_data(data);
                resp.add_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                        .unwrap_or_else(|_| panic!("header bytes")),
                );
                let _ = req.respond(resp);
            } else {
                let _ = req
                    .respond(tiny_http::Response::from_string("not found").with_status_code(404));
            }
        }
    });

    (addr, handle)
}

struct MockIssuer {
    url: String,
    _handle: thread::JoinHandle<()>,
}

impl MockIssuer {
    fn start(chatgpt_account_id: &str) -> Self {
        let (addr, handle) = start_mock_issuer(chatgpt_account_id);
        Self {
            url: issuer_url(addr),
            _handle: handle,
        }
    }
}

struct LoginServerOptionsBuilder {
    chaos_home: PathBuf,
    issuer: String,
    port: u16,
    state: String,
    forced_workspace_id: Option<String>,
}

impl LoginServerOptionsBuilder {
    fn new(chaos_home: PathBuf, issuer: impl Into<String>, state: impl Into<String>) -> Self {
        Self {
            chaos_home,
            issuer: issuer.into(),
            port: 0,
            state: state.into(),
            forced_workspace_id: None,
        }
    }

    fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    fn forced_workspace_id(mut self, forced_workspace_id: impl Into<String>) -> Self {
        self.forced_workspace_id = Some(forced_workspace_id.into());
        self
    }

    fn build(self) -> ServerOptions {
        let mut opts = ServerOptions::new(
            self.chaos_home,
            chaos_pam::CLIENT_ID.to_string(),
            self.forced_workspace_id,
            AuthCredentialsStoreMode::File,
        );
        opts.issuer = self.issuer;
        opts.port = self.port;
        opts.open_browser = false;
        opts.force_state = Some(self.state);
        opts
    }
}

struct LoginHomeFixture {
    _tmp: TempDir,
    chaos_home: PathBuf,
}

impl LoginHomeFixture {
    fn new() -> Result<Self> {
        let tmp = tempfile::tempdir()?;
        Ok(Self {
            chaos_home: tmp.path().to_path_buf(),
            _tmp: tmp,
        })
    }

    fn with_missing_subdir(subdir: &str) -> Result<Self> {
        let tmp = tempfile::tempdir()?;
        Ok(Self {
            chaos_home: tmp.path().join(subdir),
            _tmp: tmp,
        })
    }

    fn auth_path(&self) -> PathBuf {
        self.chaos_home.join("auth.json")
    }

    fn seed_auth_json(&self, auth: &chaos_kern::auth::AuthDotJson) -> Result<()> {
        save_auth(&self.chaos_home, auth, AuthCredentialsStoreMode::File)?;
        Ok(())
    }
}

#[tokio::test]
async fn end_to_end_login_flow_persists_auth_json() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let chatgpt_account_id = "12345678-0000-0000-0000-000000000000";
    let issuer = MockIssuer::start(chatgpt_account_id);
    let home = LoginHomeFixture::new()?;

    // Seed persisted auth with stale credentials that should be overwritten.
    let stale_auth = openai_auth(
        AuthMode::ApiKey,
        Some("sk-stale"),
        Some(build_tokens("stale-access", "stale-refresh")),
        None,
    );
    home.seed_auth_json(&stale_auth)?;

    let state = "test_state_123".to_string();

    // Run server in background
    let opts = LoginServerOptionsBuilder::new(home.chaos_home.clone(), issuer.url.clone(), state)
        .forced_workspace_id(chatgpt_account_id)
        .build();
    let server = run_login_server(opts)?;
    assert!(
        server
            .auth_url
            .contains(format!("allowed_workspace_id={chatgpt_account_id}").as_str()),
        "auth URL should include forced workspace parameter"
    );
    let login_port = server.actual_port;

    // Simulate browser callback, follow redirect to /success
    let client = ChaosHttpClient::default_client();
    let url = format!("http://127.0.0.1:{login_port}/auth/callback?code=abc&state=test_state_123");
    let resp = get_following_redirect(&client, &url).await?;
    assert!(resp.status().is_success());

    // Wait for server shutdown
    server.block_until_done().await?;

    let auth = load_auth_dot_json(&home.chaos_home, AuthCredentialsStoreMode::File)?
        .expect("auth should be persisted");
    let provider_record = openai_record(&auth);
    assert_eq!(provider_record.api_key.as_deref(), Some("access-123"));
    let tokens = provider_record
        .tokens
        .as_ref()
        .expect("tokens should be persisted");
    assert_eq!(tokens.access_token, "access-123");
    assert_eq!(tokens.refresh_token, "refresh-123");
    assert_eq!(tokens.account_id.as_deref(), Some(chatgpt_account_id));

    Ok(())
}

#[tokio::test]
async fn creates_missing_chaos_home_dir() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let issuer = MockIssuer::start("org-123");
    let home = LoginHomeFixture::with_missing_subdir("missing-subdir")?;

    let state = "state2".to_string();

    // Run server in background
    let opts =
        LoginServerOptionsBuilder::new(home.chaos_home.clone(), issuer.url.clone(), state).build();
    let server = run_login_server(opts)?;
    let login_port = server.actual_port;

    let client = ChaosHttpClient::default_client();
    let url = format!("http://127.0.0.1:{login_port}/auth/callback?code=abc&state=state2");
    let resp = get_following_redirect(&client, &url).await?;
    assert!(resp.status().is_success());

    server.block_until_done().await?;

    let auth_path = home.auth_path();
    assert!(
        auth_path.exists(),
        "auth.json should be created even if parent dir was missing"
    );
    Ok(())
}

#[tokio::test]
async fn forced_chatgpt_workspace_id_mismatch_blocks_login() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let issuer = MockIssuer::start("org-actual");
    let home = LoginHomeFixture::new()?;
    let state = "state-mismatch".to_string();

    let opts =
        LoginServerOptionsBuilder::new(home.chaos_home.clone(), issuer.url.clone(), state.clone())
            .forced_workspace_id("org-required")
            .build();
    let server = run_login_server(opts)?;
    assert!(
        server
            .auth_url
            .contains("allowed_workspace_id=org-required"),
        "auth URL should include forced workspace parameter"
    );
    let login_port = server.actual_port;

    let client = codex_client::ChaosHttpClient::default_client();
    let url = format!("http://127.0.0.1:{login_port}/auth/callback?code=abc&state={state}");
    let resp = client.get(&url).send().await?;
    assert!(resp.status().is_success());
    let body = resp.text().await?;
    assert!(
        body.contains("Login is restricted to workspace id org-required"),
        "error body should mention workspace restriction"
    );

    let result = server.block_until_done().await;
    assert!(
        result.is_err(),
        "login should fail due to workspace mismatch"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

    let auth_path = home.auth_path();
    assert!(
        !auth_path.exists(),
        "auth.json should not be written when the workspace mismatches"
    );

    Ok(())
}

#[tokio::test]
async fn oauth_access_denied_missing_entitlement_blocks_login_with_clear_error() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let issuer = MockIssuer::start("org-123");
    let home = LoginHomeFixture::new()?;
    let state = "state-entitlement".to_string();

    let opts =
        LoginServerOptionsBuilder::new(home.chaos_home.clone(), issuer.url.clone(), state.clone())
            .build();
    let server = run_login_server(opts)?;
    let login_port = server.actual_port;

    let client = codex_client::ChaosHttpClient::default_client();
    let url = format!(
        "http://127.0.0.1:{login_port}/auth/callback?state={state}&error=access_denied&error_description=missing_chaos_entitlement"
    );
    let resp = client.get(&url).send().await?;
    assert!(resp.status().is_success());
    let body = resp.text().await?;
    assert!(
        body.contains("You do not have access to Chaos"),
        "error body should clearly explain the Chaos access denial"
    );
    assert!(
        body.contains("Contact your workspace administrator"),
        "error body should tell the user how to get access"
    );
    assert!(
        body.contains("access_denied"),
        "error body should still include the oauth error code"
    );
    assert!(
        !body.contains("missing_chaos_entitlement"),
        "known entitlement errors should be mapped to user-facing copy"
    );

    let result = server.block_until_done().await;
    assert!(result.is_err(), "login should fail for access_denied");
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(
        err.to_string()
            .contains("Contact your workspace administrator"),
        "terminal error should also tell the user what to do next"
    );

    let auth_path = home.auth_path();
    assert!(
        !auth_path.exists(),
        "auth.json should not be written when oauth callback is denied"
    );

    Ok(())
}

#[tokio::test]
async fn oauth_access_denied_unknown_reason_uses_generic_error_page() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let issuer = MockIssuer::start("org-123");
    let home = LoginHomeFixture::new()?;
    let state = "state-generic-denial".to_string();

    let opts =
        LoginServerOptionsBuilder::new(home.chaos_home.clone(), issuer.url.clone(), state.clone())
            .build();
    let server = run_login_server(opts)?;
    let login_port = server.actual_port;

    let client = codex_client::ChaosHttpClient::default_client();
    let url = format!(
        "http://127.0.0.1:{login_port}/auth/callback?state={state}&error=access_denied&error_description=some_other_reason"
    );
    let resp = client.get(&url).send().await?;
    assert!(resp.status().is_success());
    let body = resp.text().await?;
    assert!(
        body.contains("Sign-in could not be completed"),
        "generic oauth denial should use the generic error page title"
    );
    assert!(
        body.contains("Sign-in failed: some_other_reason"),
        "generic oauth denial should preserve the oauth error details"
    );
    assert!(
        body.contains("Return to Chaos to retry"),
        "generic oauth denial should keep the generic help text"
    );
    assert!(
        body.contains("access_denied"),
        "generic oauth denial should include the oauth error code"
    );
    assert!(
        body.contains("some_other_reason"),
        "generic oauth denial should include the oauth error description"
    );
    assert!(
        !body.contains("You do not have access to Chaos"),
        "generic oauth denial should not show the entitlement-specific title"
    );
    assert!(
        !body.contains("get access to Chaos"),
        "generic oauth denial should not show the entitlement-specific admin guidance"
    );

    let result = server.block_until_done().await;
    assert!(result.is_err(), "login should fail for access_denied");
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(
        err.to_string()
            .contains("Sign-in failed: some_other_reason"),
        "terminal error should preserve generic oauth details"
    );

    let auth_path = home.auth_path();
    assert!(
        !auth_path.exists(),
        "auth.json should not be written when oauth callback is denied"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancels_previous_login_server_when_port_is_in_use() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let issuer = MockIssuer::start("org-123");
    let first_home = LoginHomeFixture::new()?;
    let first_opts = LoginServerOptionsBuilder::new(
        first_home.chaos_home.clone(),
        issuer.url.clone(),
        "cancel_state",
    )
    .build();

    let first_server = run_login_server(first_opts)?;
    let login_port = first_server.actual_port;
    let first_server_task = tokio::spawn(async move { first_server.block_until_done().await });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let second_home = LoginHomeFixture::new()?;
    let second_opts = LoginServerOptionsBuilder::new(
        second_home.chaos_home.clone(),
        issuer.url.clone(),
        "cancel_state_2",
    )
    .port(login_port)
    .build();

    let second_server = run_login_server(second_opts)?;
    assert_eq!(second_server.actual_port, login_port);

    let cancel_result = first_server_task
        .await
        .expect("first login server task panicked")
        .expect_err("login server should report cancellation");
    assert_eq!(cancel_result.kind(), io::ErrorKind::Interrupted);

    let client = codex_client::ChaosHttpClient::default_client();
    let cancel_url = format!("http://127.0.0.1:{login_port}/cancel");
    let resp = client.get(&cancel_url).send().await?;
    assert!(resp.status().is_success());

    second_server
        .block_until_done()
        .await
        .expect_err("second login server should report cancellation");
    Ok(())
}
