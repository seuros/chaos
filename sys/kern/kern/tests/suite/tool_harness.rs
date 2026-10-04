use anyhow::Context;
use chaos_ipc::plan_tool::TaskStatus;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_ipc::user_input::UserInput;
use chaos_proc::planning::{Plan, PlanChange, PlanMutation, PlanningActor};
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
use core_test_support::test_chaos::TestChaos;
use core_test_support::test_chaos::test_chaos;
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

    let mut builder = test_chaos().with_model(chaos_test_fixtures::TEST_MODEL);
    let TestChaos {
        process: chaos,
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

    chaos
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please run the shell command".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
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

    wait_for_event(&chaos, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let req = second_mock.single_request();
    let (output_text, _) = call_output(&req, call_id);
    // Shell output is reserialized to plain text before being sent to the
    // model.
    let expected_pattern = r"(?s)^Exit code: 0
Wall time: [0-9]+(?:\.[0-9]+)? seconds
Output:
tool harness
?$";
    assert_regex_match(expected_pattern, &output_text);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_progress_emits_committed_database_view() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_chaos();
    let TestChaos {
        process: chaos,
        cwd,
        session_configured,
        ..
    } = builder.build(&server).await?;
    let plan = attached_plan(&chaos, session_configured.session_id).await?;

    let call_id = "plan-tool-call";
    let plan_args = json!({
        "action": "change",
        "request_id": "start-T1",
        "change": {
            "action": "transition",
            "task": "T1",
            "task_revision": 1,
            "event": "start",
            "reason": "Inspecting the workspace"
        }
    })
    .to_string();

    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(call_id, "plan_progress", &plan_args),
        ev_completed("resp-1"),
    ]);
    responses::mount_sse_once(&server, first_response).await;

    let second_response = sse(vec![
        ev_assistant_message("msg-1", "plan acknowledged"),
        ev_completed("resp-2"),
    ]);
    let second_mock = responses::mount_sse_once(&server, second_response).await;

    let session_model = session_configured.model.clone();

    chaos
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please update the plan".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
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

    let mut saw_plan_update = false;
    wait_for_event(&chaos, |event| match event {
        EventMsg::PlanUpdate(update) => {
            saw_plan_update = true;
            assert_eq!(update.plan_id, plan.id);
            assert_eq!(update.tasks.len(), 2);
            assert_eq!(update.tasks[0].reference, "T1");
            assert_eq!(update.tasks[0].title, "Inspect workspace");
            assert_eq!(update.tasks[0].status, TaskStatus::InProgress);
            assert_eq!(update.tasks[1].title, "Report results");
            assert_eq!(update.tasks[1].status, TaskStatus::Pending);
            assert_eq!(update.tasks[1].depth, 1);
            false
        }
        EventMsg::TurnComplete(_) => true,
        _ => false,
    })
    .await;

    assert!(saw_plan_update, "expected PlanUpdate event");

    let req = second_mock.single_request();
    let (output_text, _success_flag) = call_output(&req, call_id);
    let result: chaos_proc::planning::MutationResult = serde_json::from_str(&output_text)?;
    assert_eq!(result.plan.id, plan.id);
    assert_eq!(
        result.task.context("changed task")?.status,
        TaskStatus::InProgress
    );
    let db = chaos.runtime_db().context("runtime database")?;
    assert_eq!(
        db.planning_task(&plan.id, "T1", 0).await?.task.status,
        TaskStatus::InProgress
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_progress_rejects_malformed_payload_without_ui_update() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let mut builder = test_chaos();
    let TestChaos {
        process: chaos,
        cwd,
        session_configured,
        ..
    } = builder.build(&server).await?;
    attached_plan(&chaos, session_configured.session_id).await?;

    let call_id = "plan-tool-invalid";
    let invalid_args = json!({
        "action": "change",
        "request_id": "invalid",
        "change": {"action": "transition", "task": "T1", "task_revision": 1, "event": "start"}
    })
    .to_string();

    let first_response = sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(call_id, "plan_progress", &invalid_args),
        ev_completed("resp-1"),
    ]);
    responses::mount_sse_once(&server, first_response).await;

    let second_response = sse(vec![
        ev_assistant_message("msg-1", "malformed plan payload"),
        ev_completed("resp-2"),
    ]);
    let second_mock = responses::mount_sse_once(&server, second_response).await;

    let session_model = session_configured.model.clone();

    chaos
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "please update the plan".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd.path().to_path_buf(),
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

    let mut saw_plan_update = false;
    wait_for_event(&chaos, |event| match event {
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

async fn attached_plan(
    process: &chaos_kern::Process,
    session: chaos_ipc::ProcessId,
) -> anyhow::Result<Plan> {
    let db = process.runtime_db().context("runtime database")?;
    let workspace = db.planning_create_workspace("tool harness").await?;
    let actor = PlanningActor {
        session: "operator".into(),
        installation: "test".into(),
    };
    let plan = db
        .planning_mutate(
            &actor,
            &PlanMutation {
                request_id: "create".into(),
                plan: None,
                expected_revision: None,
                change: PlanChange::Create {
                    workspace: workspace.id,
                    title: "Harness plan".into(),
                },
            },
        )
        .await?
        .plan;
    for (revision, title, parent) in [
        (1, "Inspect workspace", None),
        (2, "Report results", Some("T1".into())),
    ] {
        db.planning_mutate(
            &actor,
            &PlanMutation {
                request_id: format!("add-{revision}"),
                plan: Some(plan.id.clone()),
                expected_revision: Some(revision),
                change: PlanChange::AddTask {
                    title: title.into(),
                    parent,
                    position: 0,
                },
            },
        )
        .await?;
    }
    db.planning_attach(&session.to_string(), Some(&plan.id))
        .await?;
    Ok(plan)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_resume_reads_current_database_not_transcript() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let mut builder = test_chaos();
    let test = builder.build(&server).await?;
    let plan = attached_plan(&test.process, test.session_configured.session_id).await?;
    responses::mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "saved"),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    test.submit_turn("save this session").await?;
    test.process.submit(Op::Shutdown {}).await?;
    wait_for_event(&test.process, |event| {
        matches!(event, EventMsg::ShutdownComplete)
    })
    .await;
    let db = test.process.runtime_db().context("runtime database")?;
    db.planning_mutate(
        &PlanningActor {
            session: "other-session".into(),
            installation: "other-machine".into(),
        },
        &PlanMutation {
            request_id: "rename".into(),
            plan: Some(plan.id.clone()),
            expected_revision: Some(3),
            change: PlanChange::Edit {
                title: "Updated while offline".into(),
            },
        },
    )
    .await?;
    let resumed = builder
        .resume(&server, test.home, test.session_configured.session_id)
        .await?;
    wait_for_event(&resumed.process, |event| match event {
        EventMsg::PlanUpdate(update) => {
            assert_eq!(update.plan_id, plan.id);
            assert_eq!(update.title, "Updated while offline");
            assert_eq!(update.revision, 4);
            true
        }
        _ => false,
    })
    .await;
    resumed.process.submit(Op::Shutdown {}).await?;
    Ok(())
}
