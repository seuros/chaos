use anyhow::Context;
use anyhow::Result;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_chaos::TestChaosHarness;
use core_test_support::test_chaos::test_chaos;
use serde_json::json;
use std::time::Duration;
use tokio::time::timeout;

async fn run_tool_turn(
    harness: &TestChaosHarness,
    call_id: &str,
    tool_name: &str,
    args: serde_json::Value,
) -> Result<String> {
    let responses = vec![
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, tool_name, &serde_json::to_string(&args)?),
            ev_completed("resp-1"),
        ]),
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    ];
    mount_sse_sequence(harness.server(), responses).await;

    harness
        .submit("verify the captured shell environment")
        .await?;
    timeout(
        Duration::from_secs(20),
        harness.function_call_stdout(call_id),
    )
    .await
    .context("timed out waiting for shell environment tool output")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_command_uses_configured_environment_without_snapshot_files() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let harness =
        TestChaosHarness::with_builder(test_chaos().with_model("gpt-5.1").with_config(|config| {
            config.permissions.shell_environment_policy.r#set.insert(
                "CHAOS_ENVIRONMENT_TEST".to_string(),
                "configured".to_string(),
            );
        }))
        .await?;
    let chaos_home = harness.test().home.path().to_path_buf();

    let output = run_tool_turn(
        &harness,
        "shell-environment",
        "shell_command",
        json!({
            "command": "printf '%s' \"$CHAOS_ENVIRONMENT_TEST\"",
            "timeout_ms": 1_000,
        }),
    )
    .await?;

    assert!(output.contains("Exit code: 0"), "output={output:?}");
    assert!(output.contains("configured"), "output={output:?}");
    assert!(!chaos_home.join("shell_snapshots").exists());
    Ok(())
}

#[cfg_attr(not(target_os = "linux"), ignore)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unified_exec_uses_configured_environment_without_snapshot_files() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let harness =
        TestChaosHarness::with_builder(test_chaos().with_model("gpt-5.1").with_config(|config| {
            config.permissions.shell_environment_policy.r#set.insert(
                "CHAOS_ENVIRONMENT_TEST".to_string(),
                "configured".to_string(),
            );
        }))
        .await?;
    let chaos_home = harness.test().home.path().to_path_buf();

    let output = run_tool_turn(
        &harness,
        "unified-shell-environment",
        "exec_command",
        json!({
            "cmd": "printf '%s' \"$CHAOS_ENVIRONMENT_TEST\"",
            "yield_time_ms": 1_000,
        }),
    )
    .await?;

    assert!(output.contains("configured"), "output={output:?}");
    assert!(!chaos_home.join("shell_snapshots").exists());
    Ok(())
}
