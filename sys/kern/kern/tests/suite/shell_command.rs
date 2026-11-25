use std::time::Duration;

use anyhow::Result;
use chaos_test_fixtures::TEST_MODEL;
use core_test_support::assert_regex_match;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::ev_shell_command_call_with_args;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_chaos::TestChaosHarness;
use core_test_support::test_chaos::test_chaos;
use serde_json::json;
use test_case::test_case;

async fn mount_shell_responses(
    harness: &TestChaosHarness,
    call_id: &str,
    command: &str,
    login: Option<bool>,
    timeout: Duration,
) {
    let args = json!({
        "command": command,
        "timeout_ms": timeout.as_millis() as i64,
        "login": login,
    });

    mount_sse_sequence(
        harness.server(),
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_shell_command_call_with_args(call_id, &args),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_case(None ; "default_login")]
#[test_case(Some(true) ; "with_login")]
#[test_case(Some(false) ; "without_login")]
async fn shell_command_preserves_multiline_unicode_through_pipe(login: Option<bool>) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = TestChaosHarness::with_builder(test_chaos().with_model(TEST_MODEL)).await?;
    let call_id = "shell-pipe";
    mount_shell_responses(
        &harness,
        call_id,
        "echo 'first line\nnaïve café\nlast line' | tr 'f' 'F'",
        login,
        Duration::from_secs(5),
    )
    .await;
    harness.submit("run the pipeline").await?;

    let output = harness.function_call_stdout(call_id).await;
    let normalized = output.replace("\r\n", "\n").replace('\r', "\n");
    assert_regex_match(
        r"^Exit code: 0\nWall time: [0-9]+(?:\.[0-9]+)? seconds\nOutput:\nFirst line\nnaïve caFé\nlast line\n?$",
        &normalized,
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_command_times_out_with_timeout_ms() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = TestChaosHarness::with_builder(test_chaos().with_model(TEST_MODEL)).await?;
    let call_id = "shell-command-timeout";
    let command = "sleep 5";
    mount_shell_responses(&harness, call_id, command, None, Duration::from_millis(200)).await;
    harness
        .submit("run a long command with a short timeout")
        .await?;

    let output = harness.function_call_stdout(call_id).await;
    let normalized_output = output
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_end_matches('\n')
        .to_string();
    let expected_pattern = r"(?s)^Exit code: 124\nWall time: [0-9]+(?:\.[0-9]+)? seconds\nOutput:\ncommand timed out after [0-9]+ milliseconds\n?$";
    assert_regex_match(expected_pattern, &normalized_output);

    Ok(())
}
