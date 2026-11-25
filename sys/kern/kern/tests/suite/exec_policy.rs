#![allow(clippy::unwrap_used, clippy::expect_used)]

use anyhow::Result;
use chaos_ipc::config_types::CollaborationMode;
use chaos_ipc::config_types::ModeKind;
use chaos_ipc::config_types::Settings;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::user_input::UserInput;
use chaos_test_fixtures::TEST_MODEL;

use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_chaos::test_chaos;
use core_test_support::wait_for_event;
use serde_json::json;

fn collaboration_mode_for_model(model: String) -> CollaborationMode {
    CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort: None,
            developer_instructions: Some("exercise approvals in collaboration mode".to_string()),
        },
    }
}

async fn submit_user_turn(
    test: &core_test_support::test_chaos::TestChaos,
    prompt: &str,
    approval_policy: ApprovalPolicy,
    sandbox_policy: SandboxPolicy,
    collaboration_mode: Option<CollaborationMode>,
) -> Result<()> {
    let session_model = test.session_configured.model.clone();
    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: prompt.into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy,
            vfs_policy: chaos_ipc::permissions::VfsPolicy::from_sandbox_policy(
                &sandbox_policy,
                test.cwd_path(),
            ),
            socket_policy: chaos_ipc::permissions::SocketPolicy::from(&sandbox_policy),
            model: session_model,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode,
            personality: None,
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn execpolicy_blocks_shell_invocation() -> Result<()> {
    let home = std::sync::Arc::new(tempfile::TempDir::new()?);
    // User rules are stored in the database and can only restrict.
    chaos_kern::test_support::seed_user_rule(
        home.path(),
        "policy",
        r#"prefix_rule {pattern={"echo"}, decision="forbidden"}"#,
    )
    .await?;
    let mut builder = test_chaos().with_home(home);
    let server = start_mock_server().await;
    let test = builder.build(&server).await?;

    let call_id = "shell-forbidden";
    let args = json!({
        "command": "echo blocked",
        "timeout_ms": 1_000,
    });

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, "shell_command", &serde_json::to_string(&args)?),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    let session_model = test.session_configured.model.clone();
    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "run shell command".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            vfs_policy: chaos_ipc::permissions::VfsPolicy::unrestricted(),
            socket_policy: chaos_ipc::permissions::SocketPolicy::Enabled,
            model: session_model,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    let EventMsg::ExecCommandEnd(end) = wait_for_event(&test.process, |event| {
        matches!(event, EventMsg::ExecCommandEnd(_))
    })
    .await
    else {
        unreachable!()
    };
    wait_for_event(&test.process, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    assert!(
        end.aggregated_output
            .contains("policy forbids commands starting with `echo`"),
        "unexpected output: {}",
        end.aggregated_output
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blank_scripts_in_collaboration_mode_exit_successfully() -> Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_chaos().with_model(TEST_MODEL);
    let test = builder.build(&server).await?;

    let cases = [
        (
            "shell-empty",
            "shell_command",
            json!({"command": "", "timeout_ms": 1_000}),
        ),
        (
            "shell-whitespace",
            "shell_command",
            json!({"command": "  \n\t  ", "timeout_ms": 1_000}),
        ),
        (
            "exec-empty",
            "exec_command",
            json!({"cmd": "", "yield_time_ms": 1_000}),
        ),
        (
            "exec-whitespace",
            "exec_command",
            json!({"cmd": " \n \t", "yield_time_ms": 1_000}),
        ),
    ];
    let mut events = vec![ev_response_created("resp-1")];
    for (call_id, tool, args) in &cases {
        events.push(ev_function_call(call_id, tool, &args.to_string()));
    }
    events.push(ev_completed("resp-1"));
    mount_sse_once(&server, sse(events)).await;

    let results_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    let collaboration_mode = collaboration_mode_for_model(test.session_configured.model.clone());
    submit_user_turn(
        &test,
        "run the blank scripts with each executor",
        ApprovalPolicy::Interactive,
        SandboxPolicy::RootAccess,
        Some(collaboration_mode),
    )
    .await?;

    wait_for_event(&test.process, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let request = results_mock.single_request();
    for (call_id, tool, _) in cases {
        let output = request
            .function_call_output_text(call_id)
            .unwrap_or_else(|| panic!("missing tool output for {call_id}"));
        let success = if tool == "shell_command" {
            "Exit code: 0"
        } else {
            "Process exited with code 0"
        };
        assert!(
            output.lines().any(|line| line == success),
            "{call_id}: {output}"
        );
        assert!(
            output.trim_end().ends_with("\nOutput:"),
            "{call_id}: {output}"
        );
    }

    Ok(())
}
