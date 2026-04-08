use assert_matches::assert_matches;
use chaos_ipc::plan_tool::StepStatus;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::user_input::UserInput;
use core_test_support::assert_regex_match;
use core_test_support::responses;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_local_shell_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use serde_json::Value;
use serde_json::json;
fn call_output(req: &ResponsesRequest, call_id: &str) -> (String, Option<bool>) {
    let raw = req.function_call_output(call_id);
    assert_eq!(
        raw.get("call_id").and_then(Value::as_str),
        Some(call_id),
        "mismatched call_id in function_call_output"
    );
    let (content_opt, success) = match req.function_call_output_content_and_success(call_id) {
        Some(values) => values,
        None => panic!("function_call_output present"),
    };
    let content = match content_opt {
        Some(c) => c,
        None => panic!("function_call_output content present"),
    };
    (content, success)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_tool_executes_command_and_streams_output() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_codex().with_model("gpt-5");
    let TestCodex {
        codex,
        cwd,
        session_configured,
        ..
    } = builder.build(&server).await?;

    let call_id = "shell-tool-call";
    let command = vec!["/bin/echo", "tool harness"];
    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_local_shell_call(call_id, "completed", command),
        ev_completed("resp-1"),
    ]);
    responses::mount_sse_once(&server, first_response).await;

    let second_response = sse(vec![
        ev_assistant_message("msg-1", "all done"),
        ev_completed("resp-2"),
    ]);
    let second_mock = responses::mount_sse_once(&server, second_response).await;

    let session_model = session_configured.model.clone();

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please run the shell command".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::RootAccess,
            model: session_model,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let req = second_mock.single_request();
    let (output_text, _) = call_output(&req, call_id);
    // With freeform apply_patch as the default for all models, shell output
    // is reserialized to plain text before being sent to the model.
    let expected_pattern = r"(?s)^Exit code: 0
Wall time: [0-9]+(?:\.[0-9]+)? seconds
Output:
tool harness
?$";
    assert_regex_match(expected_pattern, &output_text);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_plan_tool_emits_plan_update_event() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_codex();
    let TestCodex {
        codex,
        cwd,
        session_configured,
        ..
    } = builder.build(&server).await?;

    let call_id = "plan-tool-call";
    let plan_args = json!({
        "explanation": "Tool harness check",
        "plan": [
            {"step": "Inspect workspace", "status": "in_progress"},
            {"step": "Report results", "status": "pending"},
        ],
    })
    .to_string();

    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(call_id, "update_plan", &plan_args),
        ev_completed("resp-1"),
    ]);
    responses::mount_sse_once(&server, first_response).await;

    let second_response = sse(vec![
        ev_assistant_message("msg-1", "plan acknowledged"),
        ev_completed("resp-2"),
    ]);
    let second_mock = responses::mount_sse_once(&server, second_response).await;

    let session_model = session_configured.model.clone();

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please update the plan".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::RootAccess,
            model: session_model,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    let mut saw_plan_update = false;
    wait_for_event(&codex, |event| match event {
        EventMsg::PlanUpdate(update) => {
            saw_plan_update = true;
            assert_eq!(update.explanation.as_deref(), Some("Tool harness check"));
            assert_eq!(update.plan.len(), 2);
            assert_eq!(update.plan[0].step, "Inspect workspace");
            assert_matches!(update.plan[0].status, StepStatus::InProgress);
            assert_eq!(update.plan[1].step, "Report results");
            assert_matches!(update.plan[1].status, StepStatus::Pending);
            false
        }
        EventMsg::TurnComplete(_) => true,
        _ => false,
    })
    .await;

    assert!(saw_plan_update, "expected PlanUpdate event");

    let req = second_mock.single_request();
    let (output_text, _success_flag) = call_output(&req, call_id);
    assert_eq!(output_text, "Plan updated");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_plan_tool_rejects_malformed_payload() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_codex();
    let TestCodex {
        codex,
        cwd,
        session_configured,
        ..
    } = builder.build(&server).await?;

    let call_id = "plan-tool-invalid";
    let invalid_args = json!({
        "explanation": "Missing plan data"
    })
    .to_string();

    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(call_id, "update_plan", &invalid_args),
        ev_completed("resp-1"),
    ]);
    responses::mount_sse_once(&server, first_response).await;

    let second_response = sse(vec![
        ev_assistant_message("msg-1", "malformed plan payload"),
        ev_completed("resp-2"),
    ]);
    let second_mock = responses::mount_sse_once(&server, second_response).await;

    let session_model = session_configured.model.clone();

    codex
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please update the plan".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            sandbox_policy: SandboxPolicy::RootAccess,
            model: session_model,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    let mut saw_plan_update = false;
    wait_for_event(&codex, |event| match event {
        EventMsg::PlanUpdate(_) => {
            saw_plan_update = true;
            false
        }
        EventMsg::TurnComplete(_) => true,
        _ => false,
    })
    .await;

    assert!(
        !saw_plan_update,
        "did not expect PlanUpdate event for malformed payload"
    );

    let req = second_mock.single_request();
    let (output_text, success_flag) = call_output(&req, call_id);
    assert!(
        output_text.contains("failed to parse function arguments"),
        "expected parse error message in output text, got {output_text:?}"
    );
    if let Some(success_flag) = success_flag {
        assert!(
            !success_flag,
            "expected tool output to mark success=false for malformed payload"
        );
    }

    Ok(())
}
