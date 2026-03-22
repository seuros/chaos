use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use codex_rmcp_client::ElicitationAction;
use codex_rmcp_client::ElicitationResponse;
use codex_rmcp_client::RmcpClient;
use codex_utils_cargo_bin::CargoBinError;
use futures::FutureExt as _;
use pretty_assertions::assert_eq;
use rmcp::model::ClientCapabilities;
use rmcp::model::CreateElicitationRequestParams;
use rmcp::model::ElicitationCapability;
use rmcp::model::FormElicitationCapability;
use rmcp::model::Implementation;
use rmcp::model::InitializeRequestParams;
use rmcp::model::ProtocolVersion;
use rmcp::model::UrlElicitationCapability;
use serde_json::json;
use tokio::sync::Mutex;

fn stdio_server_bin() -> Result<PathBuf, CargoBinError> {
    codex_utils_cargo_bin::cargo_bin("test_stdio_server")
}

fn init_params() -> InitializeRequestParams {
    InitializeRequestParams {
        meta: None,
        capabilities: ClientCapabilities {
            experimental: None,
            extensions: None,
            roots: None,
            sampling: None,
            elicitation: Some(ElicitationCapability {
                form: Some(FormElicitationCapability {
                    schema_validation: None,
                }),
                url: Some(UrlElicitationCapability {}),
            }),
            tasks: None,
        },
        client_info: Implementation {
            name: "codex-test".into(),
            version: "0.0.0-test".into(),
            title: Some("Codex URL elicitation required test".into()),
            description: None,
            icons: None,
            website_url: None,
        },
        protocol_version: ProtocolVersion::V_2025_06_18,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn url_elicitation_required_error_dispatches_url_request() -> anyhow::Result<()> {
    let client = RmcpClient::new_stdio_client(
        stdio_server_bin()?.into(),
        Vec::<OsString>::new(),
        None,
        &[],
        None,
    )
    .await?;

    let captured_requests = Arc::new(Mutex::new(Vec::new()));
    let captured_requests_clone = Arc::clone(&captured_requests);
    client
        .initialize(
            init_params(),
            Some(Duration::from_secs(5)),
            Box::new(move |_, request| {
                let captured_requests = Arc::clone(&captured_requests_clone);
                async move {
                    captured_requests.lock().await.push(request);
                    Ok(ElicitationResponse {
                        action: ElicitationAction::Accept,
                        content: None,
                        meta: None,
                    })
                }
                .boxed()
            }),
            Box::new(|_| async {}.boxed()),
            Box::new(|| async {}.boxed()),
        )
        .await?;

    let error = client
        .call_tool(
            "require_url_elicitation".to_string(),
            None,
            None,
            Some(Duration::from_secs(5)),
        )
        .await
        .expect_err("tool should still return the original URL-required error");

    let captured_requests = captured_requests.lock().await;
    assert_eq!(
        captured_requests.as_slice(),
        &[CreateElicitationRequestParams::UrlElicitationParams {
            meta: None,
            message: "Connect your account to continue.".to_string(),
            url: "https://example.test/connect".to_string(),
            elicitation_id: "elicit-123".to_string(),
        }]
    );

    let error_text = format!("{error:#}");
    assert!(error_text.contains("This request requires more information."));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn direct_url_elicitation_tool_returns_accept_after_url_request() -> anyhow::Result<()> {
    let client = RmcpClient::new_stdio_client(
        stdio_server_bin()?.into(),
        Vec::<OsString>::new(),
        None,
        &[],
        None,
    )
    .await?;

    let captured_requests = Arc::new(Mutex::new(Vec::new()));
    let captured_requests_clone = Arc::clone(&captured_requests);
    client
        .initialize(
            init_params(),
            Some(Duration::from_secs(5)),
            Box::new(move |_, request| {
                let captured_requests = Arc::clone(&captured_requests_clone);
                async move {
                    captured_requests.lock().await.push(request);
                    Ok(ElicitationResponse {
                        action: ElicitationAction::Accept,
                        content: None,
                        meta: None,
                    })
                }
                .boxed()
            }),
            Box::new(|_| async {}.boxed()),
            Box::new(|| async {}.boxed()),
        )
        .await?;

    let result = client
        .call_tool(
            "url_elicitation".to_string(),
            None,
            None,
            Some(Duration::from_secs(5)),
        )
        .await?;

    let captured_requests = captured_requests.lock().await;
    assert_eq!(
        captured_requests.as_slice(),
        &[CreateElicitationRequestParams::UrlElicitationParams {
            meta: None,
            message: "Open the connector consent page to continue.".to_string(),
            url: "https://example.test/direct-connect".to_string(),
            elicitation_id: "direct-elicit-123".to_string(),
        }]
    );
    assert_eq!(
        result,
        rmcp::model::CallToolResult::structured(json!({
            "action": "accept",
            "elicitationId": "direct-elicit-123",
        }))
    );

    Ok(())
}
