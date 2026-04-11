#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::SandboxPolicy;

use chaos_kern::sandboxing::SandboxPermissions;
use core_test_support::assert_regex_match;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_custom_tool_call;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_chaos::test_chaos;
use regex_lite::Regex;
use serde_json::Value;
use serde_json::json;

fn tool_names(body: &Value) -> Vec<String> {
    body.get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    tool.get("name")
                        .or_else(|| tool.get("type"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_tool_unknown_returns_custom_output_error() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_chaos();
    let test = builder.build(&server).await?;

    let call_id = "custom-unsupported";
    let tool_name = "unsupported_tool";

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_custom_tool_call(call_id, tool_name, "\"payload\""),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    test.submit_turn_with_policies(
        "invoke custom tool",
        ApprovalPolicy::Headless,
        SandboxPolicy::RootAccess,
    )
    .await?;

    let item = mock.single_request().custom_tool_call_output(call_id);
    let output = item
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let expected = format!("unsupported custom tool call: {tool_name}");
    assert_eq!(output, expected);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_escalated_permissions_rejected_then_ok() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_chaos().with_model("gpt-5");
    let test = builder.build(&server).await?;

    let command = ["/bin/echo", "shell ok"];
    let call_id_blocked = "shell-blocked";
    let call_id_success = "shell-success";

    let first_args = json!({
        "command": command,
        "timeout_ms": 1_000,
        "sandbox_permissions": SandboxPermissions::RequireEscalated,
    });
    let second_args = json!({
        "command": command,
        "timeout_ms": 1_000,
    });

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(
                call_id_blocked,
                "shell",
                &serde_json::to_string(&first_args)?,
            ),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let second_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-2"),
            ev_function_call(
                call_id_success,
                "shell",
                &serde_json::to_string(&second_args)?,
            ),
            ev_completed("resp-2"),
        ]),
    )
    .await;
    let third_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-3"),
        ]),
    )
    .await;

    test.submit_turn_with_policies(
        "run the shell command",
        ApprovalPolicy::Headless,
        SandboxPolicy::RootAccess,
    )
    .await?;

    let policy = ApprovalPolicy::Headless;
    let expected_message = format!(
        "approval policy is {policy:?}; reject command — you should not ask for escalated permissions if the approval policy is {policy:?}"
    );

    let blocked_output = second_mock
        .single_request()
        .function_call_output_content_and_success(call_id_blocked)
        .and_then(|(content, _)| content)
        .expect("blocked output string");
    assert_eq!(
        blocked_output, expected_message,
        "unexpected rejection message"
    );

    let success_output = third_mock
        .single_request()
        .function_call_output_content_and_success(call_id_success)
        .and_then(|(content, _)| content)
        .expect("success output string");
    // Shell output is reserialized to plain text before being sent to the
    // model.
    let expected_pattern = r"(?s)^Exit code: 0
Wall time: [0-9]+(?:\.[0-9]+)? seconds
Output:
shell ok
?$";
    assert_regex_match(expected_pattern, &success_output);

    Ok(())
}

async fn collect_tools() -> Result<Vec<String>> {
    let server = start_mock_server().await;

    let responses = vec![sse(vec![
        ev_response_created("resp-1"),
        ev_assistant_message("msg-1", "done"),
        ev_completed("resp-1"),
    ])];
    let mock = mount_sse_sequence(&server, responses).await;

    let mut builder = test_chaos();
    let test = builder.build(&server).await?;

    test.submit_turn_with_policies(
        "list tools",
        ApprovalPolicy::Headless,
        SandboxPolicy::RootAccess,
    )
    .await?;

    let first_body = mock.single_request().body_json();
    Ok(tool_names(&first_body))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unified_exec_tools_always_present() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let tools = collect_tools().await?;
    assert!(
        tools.iter().any(|name| name == "exec_command"),
        "tools list should include exec_command: {tools:?}"
    );
    assert!(
        tools.iter().any(|name| name == "write_stdin"),
        "tools list should include write_stdin: {tools:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_timeout_includes_timeout_prefix_and_metadata() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_chaos().with_model("gpt-5");
    let test = builder.build(&server).await?;

    let call_id = "shell-timeout";
    let timeout_ms = 50u64;
    let args = json!({
        "command": ["/bin/sh", "-c", "yes line | head -n 400; sleep 1"],
        "timeout_ms": timeout_ms,
    });

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, "shell", &serde_json::to_string(&args)?),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let second_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    test.submit_turn_with_policies(
        "run a long command",
        ApprovalPolicy::Headless,
        SandboxPolicy::RootAccess,
    )
    .await?;

    let timeout_item = second_mock.single_request().function_call_output(call_id);

    let output_str = timeout_item
        .get("output")
        .and_then(Value::as_str)
        .expect("timeout output string");

    // The exec path can report a timeout in three ways depending on timing
    // and whether the output gets reserialized:
    // 1) Structured JSON with exit_code 124 and a timeout prefix,
    // 2) Reserialized plain text with "Exit code: 124" and "command timed out",
    // 3) A plain error string if the child is observed as killed by a signal first.
    if let Ok(output_json) = serde_json::from_str::<Value>(output_str) {
        assert_eq!(
            output_json["metadata"]["exit_code"].as_i64(),
            Some(124),
            "expected timeout exit code 124",
        );

        let stdout = output_json["output"].as_str().unwrap_or_default();
        assert!(
            stdout.contains("command timed out"),
            "timeout output missing `command timed out`: {stdout}"
        );
    } else if output_str.contains("Exit code:") {
        // Reserialized plain-text format produced for model-facing shell
        // output.
        assert!(
            output_str.contains("Exit code: 124") || output_str.contains("command timed out"),
            "expected timeout indication in reserialized output: {output_str}"
        );
    } else {
        // Fallback: accept the signal classification path to deflake the test.
        let signal_pattern = r"(?is)^execution error:.*signal.*$";
        assert_regex_match(signal_pattern, output_str);
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_timeout_handles_background_grandchild_stdout() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_chaos().with_model("gpt-5.1").with_config(|config| {
        config
            .permissions
            .sandbox_policy
            .set(SandboxPolicy::RootAccess)
            .expect("set sandbox policy");
    });
    let test = builder.build(&server).await?;

    let call_id = "shell-grandchild-timeout";
    let pid_path = test.cwd.path().join("grandchild_pid.txt");
    let script_path = test.cwd.path().join("spawn_detached.py");
    let script = format!(
        r#"import subprocess
import time
from pathlib import Path

# Spawn a detached grandchild that inherits stdout/stderr so the pipe stays open.
proc = subprocess.Popen(["/bin/sh", "-c", "sleep 60"], start_new_session=True)
Path({pid_path:?}).write_text(str(proc.pid))
time.sleep(60)
"#
    );
    fs::write(&script_path, script)?;

    let args = json!({
        "command": ["python3", script_path.to_string_lossy()],
        "timeout_ms": 200,
    });

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, "shell", &serde_json::to_string(&args)?),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let second_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    let start = Instant::now();
    let output_str = tokio::time::timeout(Duration::from_secs(10), async {
        test.submit_turn_with_policies(
            "run a command with a detached grandchild",
            ApprovalPolicy::Headless,
            SandboxPolicy::RootAccess,
        )
        .await?;
        let timeout_item = second_mock.single_request().function_call_output(call_id);
        timeout_item
            .get("output")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("timeout output string")
    })
    .await
    .context("exec call should not hang waiting for grandchild pipes to close")??;
    let elapsed = start.elapsed();

    if let Ok(output_json) = serde_json::from_str::<Value>(&output_str) {
        assert_eq!(
            output_json["metadata"]["exit_code"].as_i64(),
            Some(124),
            "expected timeout exit code 124",
        );
    } else {
        let timeout_pattern = r"(?is)command timed out|timeout";
        assert_regex_match(timeout_pattern, &output_str);
    }

    assert!(
        elapsed < Duration::from_secs(9),
        "command should return shortly after timeout even with live grandchildren: {elapsed:?}"
    );

    if let Ok(pid_str) = fs::read_to_string(&pid_path)
        && let Ok(pid) = pid_str.trim().parse::<libc::pid_t>()
    {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_spawn_failure_truncates_exec_error() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_chaos().with_config(|cfg| {
        cfg.permissions
            .sandbox_policy
            .set(SandboxPolicy::RootAccess)
            .expect("set sandbox policy");
    });
    let test = builder.build(&server).await?;

    let call_id = "shell-spawn-failure";
    let bogus_component = "missing-bin-".repeat(700);
    let bogus_exe = test
        .cwd
        .path()
        .join(bogus_component)
        .to_string_lossy()
        .into_owned();

    let args = json!({
        "command": [bogus_exe],
        "timeout_ms": 1_000,
    });

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, "shell", &serde_json::to_string(&args)?),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let second_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    test.submit_turn_with_policies(
        "spawn a missing binary",
        ApprovalPolicy::Headless,
        SandboxPolicy::RootAccess,
    )
    .await?;

    let failure_item = second_mock.single_request().function_call_output(call_id);

    let output = failure_item
        .get("output")
        .and_then(Value::as_str)
        .expect("spawn failure output string");

    let spawn_error_pattern = r#"(?s)^Exit code: -?\d+
Wall time: [0-9]+(?:\.[0-9]+)? seconds
Output:
execution error: .*$"#;
    let spawn_truncated_pattern = r#"(?s)^Exit code: -?\d+
Wall time: [0-9]+(?:\.[0-9]+)? seconds
Total output lines: \d+
Output:

execution error: .*$"#;
    let spawn_error_regex = Regex::new(spawn_error_pattern)?;
    let spawn_truncated_regex = Regex::new(spawn_truncated_pattern)?;
    if !spawn_error_regex.is_match(output) && !spawn_truncated_regex.is_match(output) {
        let fallback_pattern = r"(?s)^execution error: .*$";
        assert_regex_match(fallback_pattern, output);
    }
    assert!(output.len() <= 10 * 1024);

    Ok(())
}
