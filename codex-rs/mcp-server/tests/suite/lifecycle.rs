use std::time::Duration;

use anyhow::Result;
use pretty_assertions::assert_eq;
use rmcp::model::ErrorCode;
use rmcp::model::JsonRpcMessage;
use rmcp::model::JsonRpcVersion2_0;
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

    assert_eq!(response.jsonrpc, JsonRpcVersion2_0);
    assert_eq!(response.result["protocolVersion"], json!("2025-06-18"));
    assert_eq!(
        response.result["capabilities"],
        json!({
            "tools": {
                "listChanged": true
            }
        })
    );
    assert_eq!(
        response.result["serverInfo"],
        json!({
            "name": "codex-mcp-server",
            "title": "Codex",
            "version": "0.0.0"
        })
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

    let JsonRpcMessage::Error(error) = message else {
        anyhow::bail!("expected JSON-RPC error, got: {message:?}");
    };

    assert_eq!(error.id, request_id);
    assert_eq!(error.error.code, ErrorCode::INVALID_REQUEST);
    assert_eq!(
        error.error.data,
        Some(json!({
            "method": "tools/list"
        }))
    );
    assert!(error.error.message.contains("before initialize"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_list_before_initialized_notification_is_rejected() -> Result<()> {
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

    let JsonRpcMessage::Error(error) = message else {
        anyhow::bail!("expected JSON-RPC error, got: {message:?}");
    };

    assert_eq!(error.id, request_id);
    assert_eq!(error.error.code, ErrorCode::INVALID_REQUEST);
    assert_eq!(
        error.error.data,
        Some(json!({
            "method": "tools/list"
        }))
    );
    assert!(
        error
            .error
            .message
            .contains("before initialized notification")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_optional_methods_return_method_not_found_after_initialize() -> Result<()> {
    let (_codex_home, mut mcp) = spawn_mcp_process().await?;
    mcp.initialize().await?;

    let requests = [
        ("resources/list", None),
        ("resources/templates/list", None),
        (
            "resources/read",
            Some(json!({ "uri": "file:///tmp/test.txt" })),
        ),
        (
            "resources/subscribe",
            Some(json!({ "uri": "file:///tmp/test.txt" })),
        ),
        (
            "resources/unsubscribe",
            Some(json!({ "uri": "file:///tmp/test.txt" })),
        ),
        ("prompts/list", None),
        ("prompts/get", Some(json!({ "name": "example" }))),
        ("logging/setLevel", Some(json!({ "level": "info" }))),
        (
            "completion/complete",
            Some(json!({
                "ref": { "type": "ref/prompt", "name": "example" },
                "argument": { "name": "topic", "value": "codex" }
            })),
        ),
    ];

    for (method, params) in requests {
        let request_id = mcp.send_custom_request(method, params).await?;
        let message = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_response_or_error_message(request_id.clone()),
        )
        .await??;

        let JsonRpcMessage::Error(error) = message else {
            anyhow::bail!("expected JSON-RPC error for `{method}`, got: {message:?}");
        };

        assert_eq!(error.id, request_id);
        assert_eq!(error.error.code, ErrorCode::METHOD_NOT_FOUND);
        assert_eq!(
            error.error.data,
            Some(json!({
                "method": method
            }))
        );
        assert_eq!(error.error.message, format!("method not found: {method}"));
    }

    Ok(())
}
