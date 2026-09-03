use std::time::Duration;

use anyhow::Result;
use chaos_cron::CreateJobParams;
use chaos_cron::CronScope;
use chaos_cron::CronStore;
use chaos_proc::open_runtime_db;
use mcp_host::protocol::types::ErrorCode;
use mcp_host::protocol::types::JsonRpcMessage;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

use mcp_test_support::McpProcess;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(20);

async fn spawn_mcp_process() -> Result<(TempDir, McpProcess)> {
    let chaos_home = TempDir::new()?;
    let mcp = McpProcess::new(chaos_home.path()).await?;
    Ok((chaos_home, mcp))
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
    // mcp-host 0.1.27's ResourcesCapability does not yet expose a
    // `listTemplates` flag, so the wire only carries `listChanged` and
    // `subscribe`. Resource templates are still served via
    // `resources/templates/list` — the capability advertisement just hasn't
    // landed upstream.
    assert_eq!(
        response.result.as_ref().unwrap()["capabilities"],
        json!({
            "tools": {
                "listChanged": true
            },
            "resources": {
                "listChanged": true,
                "subscribe": false
            },
            "tasks": {
                "list": {},
                "cancel": {},
                "requests": {
                    "tools": {
                        "call": {}
                    }
                }
            }
        })
    );
    assert_eq!(
        response.result.as_ref().unwrap()["serverInfo"],
        json!({
            "name": "chaos-mcp-server",
            "title": "FreeChaOS",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Command center for the Agents of ChaOS",
            "websiteUrl": "https://github.com/seuros/chaos"
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

    assert_eq!(resp.id, Some(request_id.to_value()));
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(
        error.data,
        Some(json!({
            "code": "not_initialized",
            "type": "validation"
        }))
    );
    assert_eq!(
        error.message,
        "Session must complete notifications/initialized first"
    );

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
    mcp.send_initialized_notification().await?;

    let request_id = mcp.send_custom_request("tools/list", None).await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(resp) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };

    assert_eq!(resp.id, Some(request_id.to_value()));
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
    assert_eq!(resp.id, Some(request_id.to_value()));
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
    assert!(uris.contains(&"chaos://spool"));
    assert!(uris.contains(&"chaos://models"));
    assert!(uris.contains(&"chaos://modes"));
    assert!(uris.contains(&"chaos://mcp"));
    assert!(uris.contains(&"chaos://man"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_resource_can_be_read_after_initialize() -> Result<()> {
    let (_chaos_home, mut mcp) = spawn_mcp_process().await?;
    mcp.initialize().await?;

    let request_id = mcp
        .send_custom_request("resources/read", Some(json!({ "uri": "chaos://mcp" })))
        .await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(response) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };
    assert_eq!(response.id, Some(request_id.to_value()));
    assert!(
        response.error.is_none(),
        "unexpected error: {:?}",
        response.error
    );

    let content = &response.result.as_ref().unwrap()["contents"][0];
    assert_eq!(content["uri"], json!("chaos://mcp"));
    assert_eq!(content["mimeType"], json!("application/json"));
    let payload: serde_json::Value =
        serde_json::from_str(content["text"].as_str().expect("MCP status text"))?;
    assert!(payload["revision"].is_null());
    assert!(payload["servers"].is_array());

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
    assert_eq!(resp.id, Some(request_id.to_value()));
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);

    let templates = resp.result.as_ref().unwrap()["resourceTemplates"]
        .as_array()
        .expect("resourceTemplates array");
    assert!(
        templates
            .iter()
            .filter_map(|template| template["uriTemplate"].as_str())
            .any(|uri_template| uri_template == "chaos://sessions/{id}")
    );
    assert!(
        templates
            .iter()
            .filter_map(|template| template["uriTemplate"].as_str())
            .any(|uri_template| uri_template == "chaos://man/{page}")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_resources_can_be_read_after_initialize() -> Result<()> {
    let (_chaos_home, mut mcp) = spawn_mcp_process().await?;
    mcp.initialize().await?;

    let index_request_id = mcp
        .send_custom_request("resources/read", Some(json!({ "uri": "chaos://man" })))
        .await?;
    let index_message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(index_request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(index_response) = index_message else {
        anyhow::bail!("expected JSON-RPC response, got: {index_message:?}");
    };
    assert_eq!(index_response.id, Some(index_request_id.to_value()));
    assert!(
        index_response.error.is_none(),
        "unexpected error: {:?}",
        index_response.error
    );

    let index_content = &index_response.result.as_ref().unwrap()["contents"][0];
    assert_eq!(index_content["uri"], json!("chaos://man"));
    assert_eq!(index_content["mimeType"], json!("application/json"));
    let index: serde_json::Value =
        serde_json::from_str(index_content["text"].as_str().expect("manual index text"))?;
    let pages = index["pages"].as_array().expect("manual pages array");
    assert_eq!(pages.len(), 4);
    assert!(
        pages
            .iter()
            .any(|page| page["uri"] == json!("chaos://man/chaos-mcp.7"))
    );

    let page_request_id = mcp
        .send_custom_request(
            "resources/read",
            Some(json!({ "uri": "chaos://man/chaos-mcp.7" })),
        )
        .await?;
    let page_message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(page_request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(page_response) = page_message else {
        anyhow::bail!("expected JSON-RPC response, got: {page_message:?}");
    };
    assert_eq!(page_response.id, Some(page_request_id.to_value()));
    assert!(
        page_response.error.is_none(),
        "unexpected error: {:?}",
        page_response.error
    );

    let page_content = &page_response.result.as_ref().unwrap()["contents"][0];
    assert_eq!(page_content["uri"], json!("chaos://man/chaos-mcp.7"));
    assert_eq!(page_content["mimeType"], json!("text/markdown"));
    let page = page_content["text"].as_str().expect("manual page text");
    assert!(page.starts_with("# chaos-mcp(7)"));
    assert!(page.contains("Index: `chaos://man`"));
    assert!(page.contains("`chaos://man/chaos-modes.7`"));
    assert!(page.contains("`chaos://man/chaos-storage.7`"));
    assert!(!page.contains("chaos-install.7"));
    assert!(!page.contains("](./"));

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
    assert_eq!(resp.id, Some(request_id.to_value()));
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
async fn spool_resource_can_be_read_after_initialize() -> Result<()> {
    let (_codex_home, mut mcp) = spawn_mcp_process().await?;
    mcp.initialize().await?;

    let request_id = mcp
        .send_custom_request("resources/read", Some(json!({ "uri": "chaos://spool" })))
        .await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(resp) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };
    assert_eq!(resp.id, Some(request_id.to_value()));
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);

    assert_eq!(
        resp.result.as_ref().unwrap(),
        &json!({
            "contents": [
                {
                    "uri": "chaos://spool",
                    "mimeType": "application/json",
                    "text": "[]"
                }
            ]
        })
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn models_resource_can_be_read_after_initialize() -> Result<()> {
    let (_codex_home, mut mcp) = spawn_mcp_process().await?;
    mcp.initialize().await?;

    let request_id = mcp
        .send_custom_request("resources/read", Some(json!({ "uri": "chaos://models" })))
        .await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(resp) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };
    assert_eq!(resp.id, Some(request_id.to_value()));
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);

    let result = resp.result.as_ref().unwrap();
    let content = &result["contents"][0];
    assert_eq!(content["uri"], json!("chaos://models"));
    assert_eq!(content["mimeType"], json!("application/json"));

    // The payload depends on which providers this machine has credentials for,
    // so assert the grouped shape rather than an exact document.
    let text = content["text"].as_str().expect("text payload");
    let groups: serde_json::Value = serde_json::from_str(text)?;
    let groups = groups.as_array().expect("providers array");
    for group in groups {
        for key in ["provider", "active", "models"] {
            assert!(
                group.get(key).is_some(),
                "provider group missing '{key}': {group}"
            );
        }
        assert!(
            group["models"].is_array(),
            "models must be an array: {group}"
        );
    }
    // Credentials never travel with the catalog, and endpoints are none of a
    // caller's business — it selects providers by id.
    for forbidden in [
        "api_key",
        "bearer",
        "token",
        "Authorization",
        "base_url",
        "wire_api",
    ] {
        assert!(
            !text.contains(forbidden),
            "payload leaked '{forbidden}': {text}"
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cron_resource_reads_jobs_from_runtime_db_even_without_preopened_state_runtime()
-> Result<()> {
    let chaos_home = TempDir::new()?;

    let pool = open_runtime_db(chaos_home.path()).await?;
    let store = CronStore::new(pool);
    store
        .create(&CreateJobParams::shell(
            "persisted job".to_string(),
            chaos_cron::Schedule::Interval { seconds: 300 }
                .to_json()
                .expect("serialize schedule"),
            "echo hi".to_string(),
            CronScope::Project,
            Some("/tmp/project".to_string()),
            None,
        ))
        .await?;

    let mut mcp = McpProcess::new(chaos_home.path()).await?;

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
    assert_eq!(resp.id, Some(request_id.to_value()));
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);

    let text = resp.result.as_ref().unwrap()["contents"][0]["text"]
        .as_str()
        .expect("cron resource text");
    let crons: serde_json::Value = serde_json::from_str(text)?;
    let items = crons.as_array().expect("cron list array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], json!("persisted job"));
    assert_eq!(items[0]["scope"], json!("project"));
    assert_eq!(items[0]["command"], json!("echo hi"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spool_resource_reads_rows_from_runtime_db_even_without_preopened_state_runtime()
-> Result<()> {
    let chaos_home = TempDir::new()?;

    let pool = open_runtime_db(chaos_home.path()).await?;
    sqlx::query(
        "INSERT INTO spool_jobs \
         (manifest_id, backend, batch_id, status, request_count, payload_json, submitted_at, created_at, updated_at) \
         VALUES (?, 'xai', 'batch-1', 'InProgress', 2, '[\"a\",\"b\"]', 123, 111, 222)",
    )
    .bind("manifest-1")
    .execute(&pool)
    .await?;

    let mut mcp = McpProcess::new(chaos_home.path()).await?;

    mcp.initialize().await?;

    let request_id = mcp
        .send_custom_request("resources/read", Some(json!({ "uri": "chaos://spool" })))
        .await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id.clone()),
    )
    .await??;

    let JsonRpcMessage::Response(resp) = message else {
        anyhow::bail!("expected JSON-RPC response, got: {message:?}");
    };
    assert_eq!(resp.id, Some(request_id.to_value()));
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);

    let text = resp.result.as_ref().unwrap()["contents"][0]["text"]
        .as_str()
        .expect("spool resource text");
    let spool: serde_json::Value = serde_json::from_str(text)?;
    let items = spool.as_array().expect("spool list array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["manifest_id"], json!("manifest-1"));
    assert_eq!(items[0]["backend"], json!("xai"));
    assert_eq!(items[0]["batch_id"], json!("batch-1"));
    assert_eq!(items[0]["status"], json!("InProgress"));

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

    assert_eq!(resp.id, Some(request_id.to_value()));
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(
        error.message,
        "Resource subscriptions are not enabled on this server"
    );

    Ok(())
}
