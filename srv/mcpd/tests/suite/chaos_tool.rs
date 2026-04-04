use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use chaos_ipc::protocol::FileChange;
use chaos_kern::spawn::CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR;
use chaos_mcpd::ApprovalElicitationAction;
use chaos_mcpd::ChaosToolParams;
use chaos_mcpd::ExecApprovalElicitRequestMeta;
use chaos_mcpd::ExecApprovalElicitRequestParams;
use chaos_mcpd::ExecApprovalResponse;
use chaos_mcpd::PatchApprovalElicitRequestMeta;
use chaos_mcpd::PatchApprovalElicitRequestParams;
use chaos_mcpd::PatchApprovalResponse;
use chaos_sh::parse_command;
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
use mcp_test_support::create_shell_command_sse_response;
use mcp_test_support::format_with_current_shell;

// Allow ample time on slower CI or under load to avoid flakes.
const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Test that a shell command that is not on the "trusted" list triggers an
/// elicitation request to the MCP and that sending the approval runs the
/// command, as expected.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_shell_command_approval_triggers_elicitation() {
    if env::var(CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok() {
        println!(
            "Skipping test because it cannot execute when network is disabled in a Codex sandbox."
        );
        return;
    }

    // Apparently `#[tokio::test]` must return `()`, so we create a helper
    // function that returns `Result` so we can use `?` in favor of `unwrap`.
    if let Err(err) = shell_command_approval_triggers_elicitation().await {
        panic!("failure: {err}");
    }
}

async fn shell_command_approval_triggers_elicitation() -> anyhow::Result<()> {
    // Use a simple, untrusted command that creates a file so we can
    // observe a side-effect.
    let workdir_for_shell_function_call = TempDir::new()?;
    let created_filename = "created_by_shell_tool.txt";
    let created_file = workdir_for_shell_function_call
        .path()
        .join(created_filename);

    let shell_command = vec!["touch".to_string(), created_filename.to_string()];
    let expected_shell_command =
        format_with_current_shell(&shlex::try_join(shell_command.iter().map(String::as_str))?);

    let McpHandle {
        process: mut mcp_process,
        server: _server,
        dir: _dir,
    } = create_mcp_process(vec![
        create_shell_command_sse_response(
            shell_command.clone(),
            Some(workdir_for_shell_function_call.path()),
            Some(5_000),
            "call1234",
        )?,
        create_final_assistant_message_sse_response("File created!")?,
    ])
    .await?;

    // Send a "codex" tool request, which should hit the responses endpoint.
    // In turn, it should reply with a tool call, which the MCP should forward
    // as an elicitation.
    let codex_request_id = mcp_process
        .send_chaos_tool_call(ChaosToolParams {
            prompt: "run `git init`".to_string(),
            ..Default::default()
        })
        .await?;
    let elicitation_request = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_request_message(),
    )
    .await??;

    assert_eq!(elicitation_request.jsonrpc, "2.0");
    assert_eq!(elicitation_request.method, "elicitation/create");

    let elicitation_request_id = elicitation_request.id.clone();
    let request_params = request_params_with_meta(&elicitation_request)?;
    let params = serde_json::from_value::<ExecApprovalElicitRequestParams>(request_params.clone())?;
    assert_eq!(
        request_params,
        create_expected_elicitation_request_params(
            expected_shell_command,
            workdir_for_shell_function_call.path(),
            codex_request_id.to_string(),
            params.meta.codex_event_id.clone(),
            params.meta.process_id,
        )?
    );

    // Accept the `git init` request by responding to the elicitation.
    let elicitation_id = elicitation_request_id
        .ok_or_else(|| anyhow::anyhow!("elicitation request should have an id"))?;
    mcp_process
        .send_response(
            RequestId::from_value(&elicitation_id)
                .ok_or_else(|| anyhow::anyhow!("invalid request id"))?,
            serde_json::to_value(ExecApprovalResponse {
                action: ApprovalElicitationAction::Accept,
                content: Some(json!({})),
                meta: None,
            })?,
        )
        .await?;

    // Verify task_complete notification arrives before the tool call completes.
    #[expect(clippy::expect_used)]
    let _task_complete = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_legacy_task_complete_notification(),
    )
    .await
    .expect("task_complete_notification timeout")
    .expect("task_complete_notification resp");

    // Verify the original `codex` tool call completes and that the file was created.
    let codex_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_response_message(RequestId::Number(codex_request_id)),
    )
    .await??;
    assert_eq!(codex_response.jsonrpc, "2.0");
    assert_eq!(codex_response.id, json!(codex_request_id));
    assert!(codex_response.error.is_none());
    let result = codex_response
        .result
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("result should be present"))?;
    assert_eq!(
        result,
        &json!({
            "structuredContent": {
                "processId": params.meta.process_id,
                "content": "File created!"
            }
        })
    );

    assert!(created_file.is_file(), "created file should exist");

    Ok(())
}

fn create_expected_elicitation_request_params(
    command: Vec<String>,
    workdir: &Path,
    codex_mcp_tool_call_id: String,
    codex_event_id: String,
    process_id: chaos_ipc::ProcessId,
) -> anyhow::Result<serde_json::Value> {
    let expected_message = format!(
        "Allow Chaos to run `{}` in `{}`?",
        shlex::try_join(command.iter().map(std::convert::AsRef::as_ref))?,
        workdir.to_string_lossy()
    );
    let codex_parsed_cmd = parse_command::parse_command(&command);
    let params_json = serde_json::to_value(ExecApprovalElicitRequestParams {
        message: expected_message,
        requested_schema: json!({"type":"object","properties":{}}),
        meta: ExecApprovalElicitRequestMeta {
            process_id,
            codex_elicitation: "exec-approval".to_string(),
            codex_mcp_tool_call_id,
            codex_event_id,
            codex_command: command,
            codex_cwd: workdir.to_path_buf(),
            codex_call_id: "call1234".to_string(),
            codex_parsed_cmd,
        },
    })?;
    Ok(params_json)
}

/// Test that patch approval triggers an elicitation request to the MCP and that
/// sending the approval applies the patch, as expected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_patch_approval_triggers_elicitation() {
    if env::var(CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok() {
        println!(
            "Skipping test because it cannot execute when network is disabled in a Codex sandbox."
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

    // Send a "codex" tool request that will trigger the apply_patch command
    let codex_request_id = mcp_process
        .send_chaos_tool_call(ChaosToolParams {
            cwd: Some(cwd.path().to_string_lossy().to_string()),
            prompt: "please modify the test file".to_string(),
            ..Default::default()
        })
        .await?;
    let elicitation_request = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_request_message(),
    )
    .await??;

    assert_eq!(elicitation_request.jsonrpc, "2.0");
    assert_eq!(elicitation_request.method, "elicitation/create");

    let elicitation_request_id = elicitation_request.id.clone();
    let request_params = request_params_with_meta(&elicitation_request)?;
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
            params.meta.codex_event_id.clone(),
            params.meta.process_id,
        )?
    );

    // Accept the patch approval request by responding to the elicitation
    let elicitation_id = elicitation_request_id
        .ok_or_else(|| anyhow::anyhow!("elicitation request should have an id"))?;
    mcp_process
        .send_response(
            RequestId::from_value(&elicitation_id)
                .ok_or_else(|| anyhow::anyhow!("invalid request id"))?,
            serde_json::to_value(PatchApprovalResponse {
                action: ApprovalElicitationAction::Accept,
                content: Some(json!({})),
                meta: None,
            })?,
        )
        .await?;

    // Verify the original `codex` tool call completes
    let codex_response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.read_stream_until_response_message(RequestId::Number(codex_request_id)),
    )
    .await??;
    assert_eq!(codex_response.jsonrpc, "2.0");
    assert_eq!(codex_response.id, json!(codex_request_id));
    assert!(codex_response.error.is_none());
    let result = codex_response
        .result
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("result should be present"))?;
    assert_eq!(
        result,
        &json!({
            "structuredContent": {
                "processId": params.meta.process_id,
                "content": "Patch has been applied successfully!"
            }
        })
    );

    let file_contents = std::fs::read_to_string(test_file.as_path())?;
    assert_eq!(file_contents, "modified content\n");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_codex_tool_passes_base_instructions() {
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

    // Send a "codex" tool request, which should hit the responses endpoint.
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
        mcp_process.read_stream_until_response_message(RequestId::Number(codex_request_id)),
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
        .context("codex tool response should include structuredContent.processId")?;
    assert_eq!(codex_response.jsonrpc, "2.0");
    assert_eq!(codex_response.id, json!(codex_request_id));
    assert_eq!(
        *result,
        json!({
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

    let developer_messages: Vec<&serde_json::Value> = request
        .get("input")
        .and_then(serde_json::Value::as_array)
        .context("responses request should include input items")?
        .iter()
        .filter(|msg| msg.get("role").and_then(|role| role.as_str()) == Some("developer"))
        .collect();
    let developer_contents: Vec<&str> = developer_messages
        .iter()
        .filter_map(|msg| msg.get("content").and_then(serde_json::Value::as_array))
        .flat_map(|content| content.iter())
        .filter(|span| span.get("type").and_then(serde_json::Value::as_str) == Some("input_text"))
        .filter_map(|span| span.get("text").and_then(serde_json::Value::as_str))
        .collect();
    assert!(
        developer_contents
            .iter()
            .any(|content| content.contains("`sandbox_mode`")),
        "expected permissions developer message, got {developer_contents:?}"
    );
    assert!(
        developer_contents.contains(&"Foreshadow upcoming tool calls."),
        "expected developer instructions in developer messages, got {developer_contents:?}"
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

fn create_expected_patch_approval_elicitation_request_params(
    changes: HashMap<PathBuf, FileChange>,
    grant_root: Option<PathBuf>,
    reason: Option<String>,
    codex_mcp_tool_call_id: String,
    codex_event_id: String,
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
            codex_event_id,
            codex_reason: reason,
            codex_grant_root: grant_root,
            codex_changes: changes,
            codex_call_id: "call1234".to_string(),
        },
    })?;

    Ok(params_json)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_shell_command_without_elicitation_capability_is_denied() {
    if env::var(CHAOS_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok() {
        println!(
            "Skipping test because it cannot execute when network is disabled in a Codex sandbox."
        );
        return;
    }

    if let Err(err) = shell_command_without_elicitation_capability_is_denied().await {
        panic!("failure: {err}");
    }
}

async fn shell_command_without_elicitation_capability_is_denied() -> anyhow::Result<()> {
    let workdir_for_shell_function_call = TempDir::new()?;
    let created_filename = "created_by_shell_tool.txt";
    let created_file = workdir_for_shell_function_call
        .path()
        .join(created_filename);

    let shell_command = vec!["touch".to_string(), created_filename.to_string()];

    let server = create_mock_responses_server(vec![
        create_shell_command_sse_response(
            shell_command.clone(),
            Some(workdir_for_shell_function_call.path()),
            Some(5_000),
            "call1234",
        )?,
        create_final_assistant_message_sse_response("Command rejected.")?,
    ])
    .await;
    let chaos_home = TempDir::new()?;
    create_config_toml(chaos_home.path(), &server.uri())?;
    let mut mcp_process = McpProcess::new(chaos_home.path()).await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp_process.initialize_without_elicitation(),
    )
    .await??;

    let codex_request_id = mcp_process
        .send_chaos_tool_call(ChaosToolParams {
            prompt: "run `git init`".to_string(),
            ..Default::default()
        })
        .await?;

    loop {
        let message = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp_process.read_next_jsonrpc_message(),
        )
        .await??;
        match message {
            JsonRpcMessage::Notification(_) => {}
            JsonRpcMessage::Request(request) => {
                panic!("unexpected elicitation request: {request:?}");
            }
            JsonRpcMessage::Response(ref resp) if resp.error.is_some() => {
                panic!("unexpected json-rpc error: {resp:?}");
            }
            JsonRpcMessage::Response(response) if response.id == json!(codex_request_id) => {
                break;
            }
            JsonRpcMessage::Response(_) => {}
        }
    }

    assert!(
        !created_file.exists(),
        "command should not have been executed"
    );

    let requests = server
        .received_requests()
        .await
        .context("mock model server should record requests")?;
    let follow_up_request = requests
        .iter()
        .find_map(|request| {
            let body = request.body_json::<serde_json::Value>().ok()?;
            let input = body.get("input")?.as_array()?;
            input.iter().find_map(|item| {
                (item.get("type").and_then(serde_json::Value::as_str)
                    == Some("function_call_output")
                    && item.get("call_id").and_then(serde_json::Value::as_str) == Some("call1234"))
                .then_some(item.clone())
            })
        })
        .ok_or_else(|| anyhow::anyhow!("missing function_call_output for denied shell command"))?;

    assert!(
        follow_up_request
            .get("output")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|output| output.contains("rejected by user")),
        "expected denied function_call_output, got {follow_up_request:?}"
    );

    Ok(())
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

/// Create a Codex config that uses the mock server as the model provider.
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

[features]
"#
        ),
    )
}
