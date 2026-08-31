use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use chaos_ipc::protocol::FileChange;
use chaos_kern::spawn::CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR;
use chaos_mcpd::ApprovalElicitationAction;
use chaos_mcpd::ChaosToolParams;
use chaos_mcpd::PatchApprovalElicitRequestMeta;
use chaos_mcpd::PatchApprovalElicitRequestParams;
use chaos_mcpd::PatchApprovalResponse;
use mcp_host::protocol::types::JsonRpcMessage;
use mcp_host::protocol::types::JsonRpcRequest;
use mcp_host::protocol::types::RequestId;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::MockServer;

use core_test_support::skip_if_no_network;
use mcp_test_support::McpProcess;
use mcp_test_support::create_apply_patch_sse_response;
use mcp_test_support::create_final_assistant_message_sse_response;
use mcp_test_support::create_mock_responses_server;

// Allow ample time on slower CI or under load to avoid flakes.
const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Test that patch approval triggers an elicitation request to the MCP and that
/// sending the approval applies the patch, as expected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_patch_approval_triggers_elicitation() {
    if env::var(CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok() {
        println!(
            "Skipping test because it cannot execute when network is disabled in a Chaos sandbox."
        );
        return;
    }

    if let Err(err) = patch_approval_triggers_elicitation().await {
        panic!("failure: {err}");
    }
}

async fn patch_approval_triggers_elicitation() -> anyhow::Result<()> {
    let cwd = TempDir::new()?;
    let test_file = cwd.path().join("destination_file.txt");
    std::fs::write(&test_file, "original content\n")?;

    let patch_content = format!(
        "*** Begin Patch\n*** Update File: {}\n-original content\n+modified content\n*** End Patch",
        test_file.as_path().to_string_lossy()
    );

    let McpHandle {
        process: mut mcp_process,
        server: _server,
        dir: _dir,
    } = create_mcp_process(vec![
        create_apply_patch_sse_response(&patch_content, "call1234")?,
        create_final_assistant_message_sse_response("Patch has been applied successfully!")?,
    ])
    .await?;

    // Send a "chaos" tool request that will trigger the apply_patch command
    let (codex_request_id, elicitation_request, request_params) =
        send_tool_call_and_read_elicitation(
            &mut mcp_process,
            ChaosToolParams {
                cwd: Some(cwd.path().to_string_lossy().to_string()),
                prompt: "please modify the test file".to_string(),
                ..Default::default()
            },
        )
        .await?;
    let params =
        serde_json::from_value::<PatchApprovalElicitRequestParams>(request_params.clone())?;

    let mut expected_changes = HashMap::new();
    expected_changes.insert(
        test_file.as_path().to_path_buf(),
        FileChange::Update {
            unified_diff: "@@ -1 +1 @@\n-original content\n+modified content\n".to_string(),
            move_path: None,
        },
    );

    assert_eq!(
        request_params,
        create_expected_patch_approval_elicitation_request_params(
            expected_changes,
            None, // No grant_root expected
            None, // No reason expected
            codex_request_id.to_string(),
            params.meta.chaos_event_id.clone(),
            params.meta.process_id,
        )?
    );

    // Accept the patch approval request by responding to the elicitation
    accept_elicitation_response(
        &mut mcp_process,
        &elicitation_request,
        &PatchApprovalResponse {
            action: ApprovalElicitationAction::Accept,
            content: Some(json!({})),
            meta: None,
        },
    )
    .await?;

    // Verify the original `chaos` tool call completes
    assert_tool_response_content(
        &mut mcp_process,
        codex_request_id,
        params.meta.process_id,
        "Patch has been applied successfully!",
    )
    .await?;

    let file_contents = std::fs::read_to_string(test_file.as_path())?;
    assert_eq!(file_contents, "modified content\n");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_chaos_tool_passes_base_instructions() {
    skip_if_no_network!();

    // Apparently `#[tokio::test]` must return `()`, so we create a helper
    // function that returns `Result` so we can use `?` in favor of `unwrap`.
    if let Err(err) = codex_tool_passes_base_instructions().await {
        panic!("failure: {err}");
    }
}

async fn codex_tool_passes_base_instructions() -> anyhow::Result<()> {
    let server =
        create_mock_responses_server(vec![create_final_assistant_message_sse_response("Enjoy!")?])
            .await;

    // Run `chaos mcp` with a specific config.toml.
    let chaos_home = TempDir::new()?;
    create_config_toml(chaos_home.path(), &server.uri())?;
    let mut mcp_process = McpProcess::new(chaos_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp_process.initialize()).await??;

    // Send a "chaos" tool request, which should hit the responses endpoint.
    let codex_request_id = mcp_process
        .send_chaos_tool_call(ChaosToolParams {
            prompt: "How are you?".to_string(),
            base_instructions: Some("You are a helpful assistant.".to_string()),
            minion_instructions: Some("Foreshadow upcoming tool calls.".to_string()),
            ..Default::default()
        })
        .await?;

    let codex_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_response_message(RequestId::Number(codex_request_id.into())),
    )
    .await??;
    let result = codex_response
        .result
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("result should be present"))?;
    let process_id = result
        .get("structuredContent")
        .and_then(|value| value.get("processId"))
        .and_then(serde_json::Value::as_str)
        .context("chaos tool response should include structuredContent.processId")?;
    assert_eq!(codex_response.jsonrpc, "2.0");
    assert_eq!(codex_response.id, Some(json!(codex_request_id)));
    let structured_text = format!("{{\"processId\":\"{process_id}\",\"content\":\"Enjoy!\"}}");
    assert_eq!(
        *result,
        json!({
            "content": [{
                "type": "text",
                "text": structured_text
            }],
            "structuredContent": {
                "processId": process_id,
                "content": "Enjoy!"
            }
        })
    );

    let requests = server
        .received_requests()
        .await
        .context("mock model server should record requests")?;
    let request = requests
        .iter()
        .find(|r| r.method == wiremock::http::Method::POST)
        .context("expected a POST request to the mock model server")?
        .body_json::<serde_json::Value>()?;
    let instructions = request
        .get("instructions")
        .and_then(serde_json::Value::as_str)
        .context("responses request should include instructions")?;
    assert!(instructions.starts_with("You are a helpful assistant."));

    let instruction_messages: Vec<&serde_json::Value> = request
        .get("input")
        .and_then(serde_json::Value::as_array)
        .context("responses request should include input items")?
        .iter()
        .filter(|msg| {
            matches!(
                msg.get("role").and_then(|role| role.as_str()),
                Some("developer" | "system")
            )
        })
        .collect();
    let instruction_contents: Vec<&str> = instruction_messages
        .iter()
        .filter_map(|msg| msg.get("content").and_then(serde_json::Value::as_array))
        .flat_map(|content| content.iter())
        .filter(|span| span.get("type").and_then(serde_json::Value::as_str) == Some("input_text"))
        .filter_map(|span| span.get("text").and_then(serde_json::Value::as_str))
        .collect();
    assert!(
        instruction_contents
            .iter()
            .any(|content| content.contains("`sandbox_mode`")),
        "expected permissions instruction message, got {instruction_contents:?}"
    );
    assert!(
        instruction_contents.contains(&"Foreshadow upcoming tool calls."),
        "expected minion instructions in instruction messages, got {instruction_contents:?}"
    );

    Ok(())
}

/// In mcp-host, `_meta` is already part of `params`, so we just return params directly.
fn request_params_with_meta(request: &JsonRpcRequest) -> anyhow::Result<serde_json::Value> {
    let params = request
        .params
        .clone()
        .ok_or_else(|| anyhow::anyhow!("elicitation request params must be set"))?;
    Ok(params)
}

async fn send_tool_call_and_read_elicitation(
    mcp_process: &mut McpProcess,
    params: ChaosToolParams,
) -> anyhow::Result<(i64, JsonRpcRequest, serde_json::Value)> {
    let codex_request_id = mcp_process.send_chaos_tool_call(params).await?;
    let elicitation_request = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_request_message(),
    )
    .await??;

    assert_eq!(elicitation_request.jsonrpc, "2.0");
    assert_eq!(elicitation_request.method, "elicitation/create");

    let request_params = request_params_with_meta(&elicitation_request)?;
    Ok((codex_request_id, elicitation_request, request_params))
}

async fn accept_elicitation_response<T: serde::Serialize>(
    mcp_process: &mut McpProcess,
    elicitation_request: &JsonRpcRequest,
    response: &T,
) -> anyhow::Result<()> {
    let elicitation_id = elicitation_request
        .id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("elicitation request should have an id"))?;
    mcp_process
        .send_response(
            RequestId::from_value(&elicitation_id)
                .ok_or_else(|| anyhow::anyhow!("invalid request id"))?,
            serde_json::to_value(response)?,
        )
        .await?;
    Ok(())
}

async fn assert_tool_response_content(
    mcp_process: &mut McpProcess,
    codex_request_id: i64,
    process_id: chaos_ipc::ProcessId,
    content: &str,
) -> anyhow::Result<()> {
    let codex_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_response_message(RequestId::Number(codex_request_id.into())),
    )
    .await??;
    assert_eq!(codex_response.jsonrpc, "2.0");
    assert_eq!(codex_response.id, Some(json!(codex_request_id)));
    assert!(codex_response.error.is_none());
    let result = codex_response
        .result
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("result should be present"))?;
    let structured_text = format!(
        "{{\"processId\":\"{process_id}\",\"content\":{}}}",
        json!(content)
    );
    assert_eq!(
        result,
        &json!({
            "content": [{
                "type": "text",
                "text": structured_text
            }],
            "structuredContent": {
                "processId": process_id,
                "content": content
            }
        })
    );
    Ok(())
}

fn create_expected_patch_approval_elicitation_request_params(
    changes: HashMap<PathBuf, FileChange>,
    grant_root: Option<PathBuf>,
    reason: Option<String>,
    codex_mcp_tool_call_id: String,
    chaos_event_id: String,
    process_id: chaos_ipc::ProcessId,
) -> anyhow::Result<serde_json::Value> {
    let mut message_lines = Vec::new();
    if let Some(r) = &reason {
        message_lines.push(r.clone());
    }
    message_lines.push("Allow Chaos to apply proposed code changes?".to_string());
    let params_json = serde_json::to_value(PatchApprovalElicitRequestParams {
        message: message_lines.join("\n"),
        requested_schema: json!({"type":"object","properties":{}}),
        meta: PatchApprovalElicitRequestMeta {
            process_id,
            codex_elicitation: "patch-approval".to_string(),
            codex_mcp_tool_call_id,
            chaos_event_id,
            codex_reason: reason,
            codex_grant_root: grant_root,
            codex_changes: changes,
            codex_call_id: "call1234".to_string(),
        },
    })?;

    Ok(params_json)
}

/// The `provider` argument has to do two things: reach `ConfigOverrides` so the
/// session actually runs against the chosen provider, and reject ids that are
/// not configured instead of silently falling back to the default.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_chaos_tool_provider_override() {
    if env::var(CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok() {
        println!(
            "Skipping test because it cannot execute when network is disabled in a Chaos sandbox."
        );
        return;
    }
    if let Err(err) = chaos_tool_provider_override().await {
        panic!("failure: {err}");
    }
}

async fn chaos_tool_provider_override() -> anyhow::Result<()> {
    let server = create_mock_responses_server(vec![create_final_assistant_message_sse_response(
        "Answered by the overridden provider.",
    )?])
    .await;
    let chaos_home = TempDir::new()?;
    // The default provider points nowhere: only an override that really lands
    // in the config can reach the mock server.
    create_two_provider_config_toml(chaos_home.path(), &server.uri())?;
    let mut mcp = McpProcess::new(chaos_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_custom_request(
            "tools/call",
            Some(json!({
                "name": "chaos",
                "arguments": {
                    "prompt": "say hi",
                    "process-id": "new",
                    "provider": "no_such_provider",
                },
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id),
    )
    .await??;
    let JsonRpcMessage::Response(response) = response else {
        anyhow::bail!("expected JSON-RPC response, got: {response:?}");
    };
    let rendered = serde_json::to_string(&response)?;
    assert!(
        rendered.contains("Model provider `no_such_provider` not found"),
        "unknown provider should be rejected rather than falling back, got {rendered}"
    );

    let request_id = mcp
        .send_custom_request(
            "tools/call",
            Some(json!({
                "name": "chaos",
                "arguments": {
                    "prompt": "say hi",
                    "process-id": "new",
                    "provider": "mock_provider",
                    "model": "mock-model",
                },
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_or_error_message(request_id),
    )
    .await??;
    let JsonRpcMessage::Response(response) = response else {
        anyhow::bail!("expected JSON-RPC response, got: {response:?}");
    };
    assert!(response.error.is_none(), "unexpected error: {response:?}");
    assert_eq!(
        response
            .result
            .as_ref()
            .and_then(|r| r.get("structuredContent"))
            .and_then(|c| c.get("content")),
        Some(&json!("Answered by the overridden provider."))
    );

    Ok(())
}

/// Default provider is deliberately unreachable so a `provider` override is the
/// only way to get a successful turn.
fn create_two_provider_config_toml(chaos_home: &Path, server_uri: &str) -> std::io::Result<()> {
    let config_toml = chaos_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
model = "dead-model"
approval_policy = "headless"
sandbox_policy = "read-only"

model_provider = "dead_provider"

[model_providers.dead_provider]
name = "Unreachable provider"
base_url = "http://127.0.0.1:1/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
"#
        ),
    )
}

/// This handle is used to ensure that the MockServer and TempDir are not dropped while
/// the McpProcess is still running.
pub struct McpHandle {
    pub process: McpProcess,
    /// Retain the server for the lifetime of the McpProcess.
    #[allow(dead_code)]
    server: MockServer,
    /// Retain the temporary directory for the lifetime of the McpProcess.
    #[allow(dead_code)]
    dir: TempDir,
}

async fn create_mcp_process(responses: Vec<String>) -> anyhow::Result<McpHandle> {
    let server = create_mock_responses_server(responses).await;
    let chaos_home = TempDir::new()?;
    create_config_toml(chaos_home.path(), &server.uri())?;
    let mut mcp_process = McpProcess::new(chaos_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp_process.initialize()).await??;
    Ok(McpHandle {
        process: mcp_process,
        server,
        dir: chaos_home,
    })
}

/// Create a Chaos config that uses the mock server as the model provider.
/// It also uses `approval_policy = "supervised"` so that we exercise the
/// elicitation code path for shell commands.
fn create_config_toml(chaos_home: &Path, server_uri: &str) -> std::io::Result<()> {
    let config_toml = chaos_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
model = "mock-model"
approval_policy = "supervised"
sandbox_policy = "workspace-write"

model_provider = "mock_provider"

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
"#
        ),
    )
}

/// Task-augmented `chaos` call: tools/call with `task` metadata returns a
/// task immediately; tasks/get reaches `completed`; tasks/result carries the
/// final tool output.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_chaos_tool_task_augmented_call() {
    if env::var(CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok() {
        println!(
            "Skipping test because it cannot execute when network is disabled in a Chaos sandbox."
        );
        return;
    }
    if let Err(err) = chaos_tool_task_augmented_call().await {
        panic!("failure: {err}");
    }
}

async fn chaos_tool_task_augmented_call() -> anyhow::Result<()> {
    let McpHandle {
        process: mut mcp,
        server: _server,
        dir: _dir,
    } = create_mcp_process(vec![create_final_assistant_message_sse_response(
        "Task path works!",
    )?])
    .await?;

    let request_id = mcp
        .send_custom_request(
            "tools/call",
            Some(json!({
                "name": "chaos",
                "arguments": { "prompt": "say hi" },
                "task": { "ttl": 60_000 }
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(request_id),
    )
    .await??;
    let task = response
        .result
        .as_ref()
        .and_then(|r| r.get("task"))
        .context("tools/call response should carry a task")?;
    assert_eq!(task.get("status"), Some(&json!("working")));
    let task_id = task
        .get("taskId")
        .and_then(|v| v.as_str())
        .context("taskId")?
        .to_string();

    let deadline = tokio::time::Instant::now() + DEFAULT_READ_TIMEOUT;
    let mut status = "working".to_string();
    while status == "working" {
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "task did not settle before timeout"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let get_id = mcp
            .send_custom_request("tasks/get", Some(json!({ "taskId": task_id })))
            .await?;
        let get_response = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_response_message(get_id),
        )
        .await??;
        status = get_response
            .result
            .as_ref()
            .and_then(|r| r.get("status"))
            .and_then(|v| v.as_str())
            .context("tasks/get status")?
            .to_string();
    }
    assert_eq!(status, "completed");

    let result_id = mcp
        .send_custom_request("tasks/result", Some(json!({ "taskId": task_id })))
        .await?;
    let result_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(result_id),
    )
    .await??;
    let result = result_response.result.context("tasks/result payload")?;
    assert_eq!(
        result
            .get("structuredContent")
            .and_then(|v| v.get("content")),
        Some(&json!("Task path works!"))
    );
    assert_eq!(
        result
            .get("_meta")
            .and_then(|m| m.get("io.modelcontextprotocol/related-task"))
            .and_then(|t| t.get("taskId"))
            .and_then(|v| v.as_str()),
        Some(task_id.as_str())
    );
    Ok(())
}
