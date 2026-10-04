//! Application update ordering and resource cleanup, not generic FSM semantics.
use super::*;
use chaos_kern::auth::AuthCredentialsStoreMode;
use std::time::Duration;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn options(home: &tempfile::TempDir) -> ServerOptions {
    let mut opts = ServerOptions::new(
        home.path().to_path_buf(),
        "login-flow-test".into(),
        None,
        AuthCredentialsStoreMode::File,
    );
    opts.open_browser = false;
    opts.port = 0;
    opts
}
async fn next(handle: &mut LoginFlowHandle) -> LoginFlowUpdate {
    tokio::time::timeout(Duration::from_secs(5), handle.recv())
        .await
        .unwrap_or_else(|_| panic!("login update timed out"))
        .unwrap_or_else(|| panic!("login update stream closed early"))
}

#[tokio::test]
async fn cancellation_before_first_poll_is_not_lost_and_emits_only_cancelled() {
    let home = tempfile::tempdir().unwrap();
    let (mut handle, driver) = start_login_flow(options(&home), LoginFlowMode::Browser);
    handle.cancel();
    assert!(matches!(
        next(&mut handle).await,
        LoginFlowUpdate::Cancelled
    ));
    assert!(handle.recv().await.is_none());
    driver.await.unwrap();
}

#[tokio::test]
async fn callback_server_remembers_shutdown_before_its_first_poll() {
    let home = tempfile::tempdir().unwrap();
    let server = run_login_server(options(&home)).unwrap();
    server.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), server.block_until_done())
        .await
        .unwrap();
    assert!(result.is_err());
}

#[tokio::test]
async fn browser_cancellation_stops_the_listener_and_closes_the_update_stream() {
    let home = tempfile::tempdir().unwrap();
    let (mut handle, driver) = start_login_flow(options(&home), LoginFlowMode::Browser);
    let LoginFlowUpdate::BrowserOpened { actual_port, .. } = next(&mut handle).await else {
        panic!("expected browser listener");
    };
    handle.cancel();
    assert!(matches!(
        next(&mut handle).await,
        LoginFlowUpdate::Cancelled
    ));
    assert!(handle.recv().await.is_none());
    driver.await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if std::net::TcpListener::bind(("127.0.0.1", actual_port)).is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unsupported_device_flow_fails_without_browser_when_not_permitted() {
    let home = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    let mut opts = options(&home);
    opts.issuer = server.uri();
    let (mut handle, driver) = start_login_flow(
        opts,
        LoginFlowMode::DeviceCode {
            allow_browser_fallback: false,
        },
    );
    assert!(matches!(
        next(&mut handle).await,
        LoginFlowUpdate::DeviceCodePending
    ));
    assert!(matches!(
        next(&mut handle).await,
        LoginFlowUpdate::Failed { .. }
    ));
    assert!(handle.recv().await.is_none());
    driver.await.unwrap();
}

#[tokio::test]
async fn permitted_device_fallback_emits_unsupported_before_browser() {
    let home = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    let mut opts = options(&home);
    opts.issuer = server.uri();
    let (mut handle, driver) = start_login_flow(
        opts,
        LoginFlowMode::DeviceCode {
            allow_browser_fallback: true,
        },
    );
    assert!(matches!(
        next(&mut handle).await,
        LoginFlowUpdate::DeviceCodePending
    ));
    assert!(matches!(
        next(&mut handle).await,
        LoginFlowUpdate::DeviceCodeUnsupported
    ));
    assert!(matches!(
        next(&mut handle).await,
        LoginFlowUpdate::BrowserOpened { .. }
    ));
    handle.cancel();
    assert!(matches!(
        next(&mut handle).await,
        LoginFlowUpdate::Cancelled
    ));
    assert!(handle.recv().await.is_none());
    driver.await.unwrap();
}
