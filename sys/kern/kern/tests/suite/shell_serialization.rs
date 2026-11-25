#![allow(clippy::expect_used)]

use anyhow::Result;
use chaos_ipc::protocol::SandboxPolicy;
use core_test_support::assert_regex_match;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_local_shell_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_chaos::ApplyPatchModelOutput;
use core_test_support::test_chaos::ShellModelOutput;
use core_test_support::test_chaos::TestChaosBuilder;
use core_test_support::test_chaos::test_chaos;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::fs;
use test_case::test_case;

use super::apply_patch_cli::apply_patch_harness;
use super::apply_patch_cli::mount_apply_patch;

const FIXTURE_JSON: &str = r#"{
    "description": "This is an example JSON file.",
    "foo": "bar",
    "isTest": true,
    "testNumber": 123,
    "testArray": [1, 2, 3],
    "testObject": {
        "foo": "bar"
    }
}
"#;

fn shell_responses(
    call_id: &str,
    command: Vec<&str>,
    output_type: ShellModelOutput,
) -> Result<Vec<String>> {
    match output_type {
        ShellModelOutput::ShellCommand => {
            let command = shlex::try_join(command)?;
            let parameters = json!({
                "command": command,
                "timeout_ms": 2_000,
            });
            Ok(vec![
                sse(vec![
                    ev_response_created("resp-1"),
                    ev_function_call(
                        call_id,
                        "shell_command",
                        &serde_json::to_string(&parameters)?,
                    ),
                    ev_completed("resp-1"),
                ]),
                sse(vec![
                    ev_assistant_message("msg-1", "done"),
                    ev_completed("resp-2"),
                ]),
            ])
        }
        ShellModelOutput::Shell => {
            let parameters = json!({
                "command": command,
                "timeout_ms": 2_000,
            });
            Ok(vec![
                sse(vec![
                    ev_response_created("resp-1"),
                    ev_function_call(call_id, "shell", &serde_json::to_string(&parameters)?),
                    ev_completed("resp-1"),
                ]),
                sse(vec![
                    ev_assistant_message("msg-1", "done"),
                    ev_completed("resp-2"),
                ]),
            ])
        }
        ShellModelOutput::LocalShell => Ok(vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_local_shell_call(call_id, "completed", command),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ]),
    }
}

fn configure_shell_model(
    builder: TestChaosBuilder,
    output_type: ShellModelOutput,
    include_apply_patch_tool: bool,
) -> TestChaosBuilder {
    match (output_type, include_apply_patch_tool) {
        (ShellModelOutput::ShellCommand, _) => builder.with_model("test-gpt-5-codex"),
        (ShellModelOutput::LocalShell, true) => builder.with_model("gpt-5.1-codex"),
        (ShellModelOutput::Shell, true) => builder.with_model("gpt-5.1-codex"),
        (ShellModelOutput::LocalShell, false) => builder.with_model("chaos-mini-latest"),
        (ShellModelOutput::Shell, false) => builder.with_model("gpt-5"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_case(ShellModelOutput::Shell, false)]
#[test_case(ShellModelOutput::LocalShell, false)]
#[test_case(ShellModelOutput::Shell, true)]
#[test_case(ShellModelOutput::ShellCommand, true)]
#[test_case(ShellModelOutput::LocalShell, true)]
async fn shell_output_reserializes_fixture_json_as_plain_text(
    output_type: ShellModelOutput,
    include_apply_patch_tool: bool,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = configure_shell_model(test_chaos(), output_type, include_apply_patch_tool);
    let test = builder.build(&server).await?;

    let fixture_path = test.cwd.path().join("fixture.json");
    fs::write(&fixture_path, FIXTURE_JSON)?;
    let fixture_path_str = fixture_path.to_string_lossy().to_string();

    let call_id = "shell-json-fixture";
    let responses = shell_responses(
        call_id,
        vec!["/usr/bin/sed", "-n", "p", fixture_path_str.as_str()],
        output_type,
    )?;
    let mock = mount_sse_sequence(&server, responses).await;

    test.submit_turn_with_policy("read the fixture JSON with sed", SandboxPolicy::RootAccess)
        .await?;

    let req = mock.last_request().expect("shell output request recorded");
    let output_item = req.function_call_output(call_id);
    let output = output_item
        .get("output")
        .and_then(Value::as_str)
        .expect("shell output string");

    let (header, body) = output
        .split_once("\nOutput:\n")
        .expect("structured output contains an Output section");
    assert_regex_match(
        r"(?s)^Exit code: 0\nWall time: [0-9]+(?:\.[0-9]+)? seconds$",
        header,
    );
    assert_eq!(
        body, FIXTURE_JSON,
        "expected Output section to include the fixture contents"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_case(ShellModelOutput::Shell)]
#[test_case(ShellModelOutput::ShellCommand)]
#[test_case(ShellModelOutput::LocalShell)]
async fn shell_output_for_apply_patch_enabled_records_duration(
    output_type: ShellModelOutput,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = configure_shell_model(test_chaos(), output_type, true);
    let test = builder.build(&server).await?;

    let call_id = "shell-structured";
    let responses = shell_responses(call_id, vec!["/bin/sh", "-c", "sleep 0.2"], output_type)?;
    let mock = mount_sse_sequence(&server, responses).await;

    test.submit_turn_with_policy(
        "run the structured shell command",
        SandboxPolicy::RootAccess,
    )
    .await?;

    let req = mock
        .last_request()
        .expect("structured output request recorded");
    let output_item = req.function_call_output(call_id);
    let output = output_item
        .get("output")
        .and_then(Value::as_str)
        .expect("structured output string");

    let expected_pattern = r#"(?s)^Exit code: 0
Wall time: ([0-9]+(?:\.[0-9]+)?) seconds
Output:
$"#;
    let captures = assert_regex_match(expected_pattern, output);
    let wall_time_seconds: f32 = captures[1].parse()?;
    assert!(
        wall_time_seconds > 0.1,
        "expected wall time to be greater than zero seconds, got {wall_time_seconds}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_case(ApplyPatchModelOutput::Freeform)]
#[test_case(ApplyPatchModelOutput::Function)]
#[test_case(ApplyPatchModelOutput::Shell)]
#[test_case(ApplyPatchModelOutput::ShellViaHeredoc)]
async fn apply_patch_custom_tool_call_creates_file(
    output_type: ApplyPatchModelOutput,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = apply_patch_harness().await?;

    let call_id = "apply-patch-add-file";
    let file_name = "custom_tool_apply_patch.txt";
    let patch = format!(
        "*** Begin Patch\n*** Add File: {file_name}\n+custom tool content\n*** End Patch\n"
    );
    mount_apply_patch(&harness, call_id, &patch, "apply_patch done", output_type).await;

    harness
        .test()
        .submit_turn_with_policy(
            "apply the patch via custom tool to create a file",
            SandboxPolicy::RootAccess,
        )
        .await?;

    let output = harness.apply_patch_output(call_id, output_type).await;

    let expected_pattern = format!(
        r"(?s)^Exit code: 0
Wall time: [0-9]+(?:\.[0-9]+)? seconds
Output:
Success. Updated the following files:
A (?:.+/)?{file_name}
?$"
    );
    assert_regex_match(&expected_pattern, output.as_str());

    let new_file_path = harness.path(file_name);
    let created_contents = fs::read_to_string(&new_file_path)?;
    assert_eq!(
        created_contents, "custom tool content\n",
        "expected file contents for {file_name}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_case(ApplyPatchModelOutput::Freeform)]
#[test_case(ApplyPatchModelOutput::Function)]
#[test_case(ApplyPatchModelOutput::Shell)]
#[test_case(ApplyPatchModelOutput::ShellViaHeredoc)]
async fn apply_patch_custom_tool_call_updates_existing_file(
    output_type: ApplyPatchModelOutput,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = apply_patch_harness().await?;

    let call_id = "apply-patch-update-file";
    let file_name = "custom_tool_apply_patch_existing.txt";
    let file_path = harness.path(file_name);
    fs::write(&file_path, "before\n")?;
    let patch = format!(
        "*** Begin Patch\n*** Update File: {file_name}\n@@\n-before\n+after\n*** End Patch\n"
    );
    mount_apply_patch(
        &harness,
        call_id,
        &patch,
        "apply_patch update done",
        output_type,
    )
    .await;

    harness
        .test()
        .submit_turn_with_policy(
            "apply the patch via custom tool to update a file",
            SandboxPolicy::RootAccess,
        )
        .await?;

    let output = harness.apply_patch_output(call_id, output_type).await;

    let expected_pattern = format!(
        r"(?s)^Exit code: 0
Wall time: [0-9]+(?:\.[0-9]+)? seconds
Output:
Success. Updated the following files:
M (?:.+/)?{file_name}
?$"
    );
    assert_regex_match(&expected_pattern, output.as_str());

    let updated_contents = fs::read_to_string(file_path)?;
    assert_eq!(updated_contents, "after\n", "expected updated file content");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_case(ApplyPatchModelOutput::Freeform)]
#[test_case(ApplyPatchModelOutput::Function)]
#[test_case(ApplyPatchModelOutput::Shell)]
#[test_case(ApplyPatchModelOutput::ShellViaHeredoc)]
async fn apply_patch_custom_tool_call_reports_failure_output(
    output_type: ApplyPatchModelOutput,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = apply_patch_harness().await?;

    let call_id = "apply-patch-failure";
    let missing_file = "missing_custom_tool_apply_patch.txt";
    let patch = format!(
        "*** Begin Patch\n*** Update File: {missing_file}\n@@\n-before\n+after\n*** End Patch\n"
    );
    mount_apply_patch(
        &harness,
        call_id,
        &patch,
        "apply_patch failure done",
        output_type,
    )
    .await;

    harness
        .test()
        .submit_turn_with_policy(
            "attempt a failing apply_patch via custom tool",
            SandboxPolicy::RootAccess,
        )
        .await?;

    let output = harness.apply_patch_output(call_id, output_type).await;

    let expected_output = format!(
        "apply_patch verification failed: Failed to read file to update {}/{missing_file}: No such file or directory (os error 2)",
        harness.cwd().to_string_lossy()
    );
    assert_eq!(output, expected_output.as_str());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_case(ShellModelOutput::Shell)]
#[test_case(ShellModelOutput::ShellCommand)]
#[test_case(ShellModelOutput::LocalShell)]
async fn shell_output_is_structured_for_nonzero_exit(output_type: ShellModelOutput) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_chaos().with_model("gpt-5.1-codex");
    let test = builder.build(&server).await?;

    let call_id = "shell-nonzero-exit";
    let responses = shell_responses(call_id, vec!["/bin/sh", "-c", "exit 42"], output_type)?;
    let mock = mount_sse_sequence(&server, responses).await;

    test.submit_turn_with_policy("run the failing shell command", SandboxPolicy::RootAccess)
        .await?;

    let req = mock.last_request().expect("shell output request recorded");
    let output_item = req.function_call_output(call_id);
    let output = output_item
        .get("output")
        .and_then(Value::as_str)
        .expect("shell output string");

    let expected_pattern = r"(?s)^Exit code: 42
Wall time: [0-9]+(?:\.[0-9]+)? seconds
Output:
?$";
    assert_regex_match(expected_pattern, output);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_command_output_is_freeform() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_chaos();
    let test = builder.build(&server).await?;

    let call_id = "shell-command";
    let args = json!({
        "command": "echo shell command",
        "login": false,
        "timeout_ms": 1_000,
    });
    let responses = vec![
        sse(vec![
            json!({"type": "response.created", "response": {"id": "resp-1"}}),
            ev_function_call(call_id, "shell_command", &serde_json::to_string(&args)?),
            ev_completed("resp-1"),
        ]),
        sse(vec![
            ev_assistant_message("msg-1", "shell_command done"),
            ev_completed("resp-2"),
        ]),
    ];
    let mock = mount_sse_sequence(&server, responses).await;

    test.submit_turn_with_policy(
        "run the shell_command script in the user's shell",
        SandboxPolicy::RootAccess,
    )
    .await?;

    let req = mock
        .last_request()
        .expect("shell_command output request recorded");
    let output_item = req.function_call_output(call_id);
    let output = output_item
        .get("output")
        .and_then(Value::as_str)
        .expect("shell_command output string");

    let expected_pattern = r"(?s)^Exit code: 0
Wall time: [0-9]+(?:\.[0-9]+)? seconds
Output:
shell command
?$";
    assert_regex_match(expected_pattern, output);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_case(10_000, false ; "at_limit")]
#[test_case(10_001, true ; "one_byte_over")]
async fn shell_command_output_at_byte_limit(bytes: usize, truncated: bool) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_chaos().with_model("gpt-5.1");
    let test = builder.build(&server).await?;

    let call_id = "shell-command";
    let args = json!({
        "command": format!("printf '%*s' {bytes} '' | tr ' ' 1"),
        "login": false,
        "timeout_ms": 1000,
    });
    let responses = vec![
        sse(vec![
            json!({"type": "response.created", "response": {"id": "resp-1"}}),
            ev_function_call(call_id, "shell_command", &serde_json::to_string(&args)?),
            ev_completed("resp-1"),
        ]),
        sse(vec![
            ev_assistant_message("msg-1", "shell_command done"),
            ev_completed("resp-2"),
        ]),
    ];
    let mock = mount_sse_sequence(&server, responses).await;

    test.submit_turn_with_policy(
        "run the shell_command script in the user's shell",
        SandboxPolicy::RootAccess,
    )
    .await?;

    let req = mock
        .last_request()
        .expect("shell_command output request recorded");
    let output_item = req.function_call_output(call_id);
    let output = output_item
        .get("output")
        .and_then(Value::as_str)
        .expect("shell_command output string");

    let (header, body) = output.split_once("\nOutput:\n").expect("output section");
    assert_regex_match(
        r"^Exit code: 0\nWall time: [0-9]+(?:\.[0-9]+)? seconds$",
        header,
    );
    let expected = if truncated {
        let half = "1".repeat(5_000);
        format!("{half}…1 chars truncated…{half}")
    } else {
        "1".repeat(10_000)
    };
    assert_eq!(body, expected);

    Ok(())
}
