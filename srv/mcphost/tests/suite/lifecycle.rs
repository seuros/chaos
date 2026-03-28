use std::time::Duration;

use anyhow::Result;
use mcp_host::protocol::types::ErrorCode;
use mcp_host::protocol::types::JsonRpcMessage;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

use mcp_test_support::McpProcess;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(20);

async fn spawn_mcp_process() -> Result<(TempDir, McpProcess)> {
    let codex_home = TempDir::new()?;
    let mcp = McpProcess::new(codex_home.path()).await?;
    Ok((codex_home, mcp))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_negotiates_newer_client_protocol_to_latest_supported_version() -> Result<()> {
    let (_codex_home, mut mcp) = spawn_mcp_process().await?;

    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.initialize_with_protocol_version("2025-11-25"),
    )
    .await??;

    let JsonRpcMessage::Response(response) = message else {
        anyhow::bail!("expected initialize response, got: {message:?}");
    };

    assert_eq!(response.jsonrpc, "2.0");
    assert_eq!(
        response.result.as_ref().unwrap()["protocolVersion"],
        json!("2025-11-25")
    );
    assert_eq!(
        response.result.as_ref().unwrap()["capabilities"],
        json!({
            "tools": {
                "listChanged": true
            },
            "resources": {
                "listChanged": true,
                "subscribe": false,
                "listTemplates": true
            }
        })
    );
    assert_eq!(
        response.result.as_ref().unwrap()["serverInfo"],
        json!({
            "name": "chaos-mcp-server",
            "version": env!("CARGO_PKG_VERSION")
        })
    );
    assert_eq!(
        response.result.as_ref().unwrap()["instructions"],
        json!("Chaos — provider-agnostic coding agent")
    );

    mcp.send_initialized_notification().await?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_list_before_initialize_is_rejected() -> Result<()> {
    let (_codex_home, mut mcp) = spawn_mcp_process().await?;

    let request_id = mcp.send_custom_request("tools/list", None).await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(resp) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };
    let error = resp.error.as_ref().expect("expected error response");

    assert_eq!(resp.id, request_id.to_value());
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(
        error.data,
        Some(json!({
            "code": "not_initialized",
            "type": "validation"
        }))
    );
    assert_eq!(error.message, "Session must be initialized first");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_list_succeeds_after_initialize_response() -> Result<()> {
    let (_codex_home, mut mcp) = spawn_mcp_process().await?;

    let _ = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.initialize_with_protocol_version("2025-11-25"),
    )
    .await??;

    let request_id = mcp.send_custom_request("tools/list", None).await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(resp) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };

    assert_eq!(resp.id, request_id.to_value());
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
    let tools = resp.result.as_ref().unwrap()["tools"]
        .as_array()
        .expect("tools array");
    assert!(
        !tools.is_empty(),
        "tools/list should succeed immediately after initialize"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resources_are_listed_after_initialize() -> Result<()> {
    let (_codex_home, mut mcp) = spawn_mcp_process().await?;
    mcp.initialize().await?;

    let request_id = mcp.send_custom_request("resources/list", None).await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(resp) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };
    assert_eq!(resp.id, request_id.to_value());
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);

    let resources = resp.result.as_ref().unwrap()["resources"]
        .as_array()
        .expect("resources array");
    let uris: Vec<&str> = resources
        .iter()
        .filter_map(|resource| resource["uri"].as_str())
        .collect();
    assert!(uris.contains(&"chaos://sessions"));
    assert!(uris.contains(&"chaos://crons"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_templates_are_listed_after_initialize() -> Result<()> {
    let (_codex_home, mut mcp) = spawn_mcp_process().await?;
    mcp.initialize().await?;

    let request_id = mcp
        .send_custom_request("resources/templates/list", None)
        .await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(resp) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };
    assert_eq!(resp.id, request_id.to_value());
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);

    let templates = resp.result.as_ref().unwrap()["resourceTemplates"]
        .as_array()
        .expect("resourceTemplates array");
    let uri_templates: Vec<&str> = templates
        .iter()
        .filter_map(|template| template["uriTemplate"].as_str())
        .collect();
    assert!(uri_templates.contains(&"chaos://sessions/{id}"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cron_resource_can_be_read_after_initialize() -> Result<()> {
    let (_codex_home, mut mcp) = spawn_mcp_process().await?;
    mcp.initialize().await?;

    let request_id = mcp
        .send_custom_request("resources/read", Some(json!({ "uri": "chaos://crons" })))
        .await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(resp) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };
    assert_eq!(resp.id, request_id.to_value());
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);

    assert_eq!(
        resp.result.as_ref().unwrap(),
        &json!({
            "contents": [
                {
                    "uri": "chaos://crons",
                    "mimeType": "application/json",
                    "text": "[]"
                }
            ]
        })
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_subscribe_is_rejected_when_capability_disabled() -> Result<()> {
    let (_codex_home, mut mcp) = spawn_mcp_process().await?;
    mcp.initialize().await?;

    let request_id = mcp
        .send_custom_request(
            "resources/subscribe",
            Some(json!({ "uri": "file:///tmp/test.txt" })),
        )
        .await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(resp) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };
    let error = resp.error.as_ref().expect("expected error response");

    assert_eq!(resp.id, request_id.to_value());
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(
        error.message,
        "Resource subscriptions are not enabled on this server"
    );

    Ok(())
}
