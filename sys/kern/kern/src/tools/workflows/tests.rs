use super::*;
use crate::tools::context::ToolPayload;
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::router::ToolRouter;
use crate::turn_diff_tracker::TurnDiffTracker;
use chaos_synopsis::runs::RunStatus;

#[tokio::test]
async fn explicit_user_input_call_requires_the_origin_aware_ask_bridge() -> anyhow::Result<()> {
    let (session, turn, _) = fixture().await;
    let cancellation = tokio_util::sync::CancellationToken::new();
    let router = crate::chaos::built_tools(&session, &turn, &[], &cancellation).await?;
    for name in [
        "request_user_input",
        "functions.request_user_input",
        "mcp__other__request_user_input",
    ] {
        let root = serde_json::from_value(json!({
            "type":"call","id":"question","tool":name,"arguments":{}
        }))?;
        let Err(error) = dispatch::prepare(root, &router) else {
            anyhow::bail!("explicit input call must not select a native mailbox");
        };
        assert!(error.to_string().contains("use an ask leaf"), "{error}");
    }
    Ok(())
}

#[test]
fn workflow_output_keeps_payload_out_of_parent_logs() {
    use crate::tools::context::ToolOutput;
    let output = WorkflowOutput {
        text: r#"{"secret":"do-not-log"}"#.into(),
        success: true,
    };
    assert!(!output.log_preview().contains("do-not-log"));
    let reply = output.to_response_item(
        "call",
        &ToolPayload::Function {
            arguments: "{}".into(),
        },
    );
    assert!(serde_json::to_string(&reply).is_ok_and(|value| value.contains("do-not-log")));
}

#[tokio::test]
async fn native_leaf_uses_existing_dispatch_without_container_deadlock() -> anyhow::Result<()> {
    let (session, turn, tracker) = fixture().await;
    session.services.tool_group_catalog.set_groups_enabled(
        &session.services.tool_group_state,
        [
            crate::tools::groups::WORKFLOWS,
            crate::tools::groups::FILESYSTEM,
        ],
        true,
    )?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("workflow_fixture.txt");
    std::fs::write(&path, "workflow fixture\n")?;
    let cancellation = tokio_util::sync::CancellationToken::new();
    let router = crate::chaos::built_tools(&session, &turn, &[], &cancellation).await?;
    let call = crate::tools::router::ToolCall {
        tool_name: "run_workflow".into(),
        tool_namespace: None,
        call_id: "outer".into(),
        payload: ToolPayload::Function {
            arguments: json!({"tree":{
                "type":"sequence","id":"root","children":[
                    {
                        "type":"call","id":"read","tool":"read_file",
                        "arguments":{"file_path":path,"limit":2}
                    },
                    {"type":"condition","id":"check","pointer":"/read","predicate":{"op":"exists"}}
                ]
            }})
            .to_string(),
        },
    };
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        ToolCallRuntime::new(router, session, turn, tracker).handle_tool_call(call, cancellation),
    )
    .await??;
    let chaos_ipc::models::ResponseInputItem::FunctionCallOutput { output, .. } = result else {
        anyhow::bail!("unexpected workflow output");
    };
    let snapshot: Value = serde_json::from_str(
        &output
            .body
            .to_text()
            .ok_or_else(|| anyhow::anyhow!("missing output"))?,
    )?;
    assert_eq!(
        snapshot["status"],
        json!({"state":"terminal","outcome":"success"}),
        "{snapshot}"
    );
    assert_eq!(snapshot["steps"].as_array().map(Vec::len), Some(2));
    assert_eq!(snapshot["steps"][0]["detail"]["tool"], "read_file");
    assert_eq!(snapshot["result"]["steps"][0]["tool"], "read_file");
    assert!(
        snapshot["steps"][0]["detail"]["result"]
            .to_string()
            .contains("workflow fixture")
    );
    Ok(())
}

#[test]
fn canonical_display_does_not_rewrite_user_output() -> anyhow::Result<()> {
    let registry = chaos_synopsis::runs::RunRegistry::default();
    let lease = registry.start(RunKind::Workflow)?;
    let observer = observer::Observer::new(
        lease.recorder(),
        lease.cancellation(),
        BTreeMap::from([("leaf_0".into(), "read_file".into())]),
    );
    let mut result = json!({"steps":[
        {"tool":"leaf_0","result":{"tool":"leaf_0","steps":[{"tool":"leaf_0"}]}},
        {"ask":"leaf_0","content":{"tool":"leaf_0"}}
    ]});
    observer.display_result(&mut result);
    assert_eq!(result["steps"][0]["tool"], "read_file");
    assert_eq!(result["steps"][0]["result"]["tool"], "leaf_0");
    assert_eq!(result["steps"][0]["result"]["steps"][0]["tool"], "leaf_0");
    assert_eq!(result["steps"][1]["ask"], "leaf_0");
    assert_eq!(result["steps"][1]["content"]["tool"], "leaf_0");
    Ok(())
}

pub(super) async fn fixture() -> (
    Arc<crate::chaos::Session>,
    Arc<crate::chaos::TurnContext>,
    crate::tools::context::SharedTurnDiffTracker,
) {
    let (session, turn) = crate::chaos::make_session_and_context().await;
    (
        Arc::new(session),
        Arc::new(turn),
        Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
    )
}

#[tokio::test]
async fn native_ask_uses_turn_input_and_validates_answer() -> anyhow::Result<()> {
    use chaos_ipc::request_user_input::{RequestUserInputAnswer, RequestUserInputResponse};
    let (session, turn, tracker) = fixture().await;
    session.services.tool_group_catalog.set_groups_enabled(
        &session.services.tool_group_state,
        [
            crate::tools::groups::WORKFLOWS,
            crate::tools::groups::SESSION,
        ],
        true,
    )?;
    let active = crate::state::ActiveTurn::default();
    let state = active.turn_state.clone();
    *session.active_turn.lock().await = Some(active);
    let turn_id = turn.sub_id.clone();
    let task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        WorkflowHandler
            .handle(invocation(
                session,
                turn,
                tracker,
                "run_workflow",
                json!({"tree":{"type":"sequence","id":"root","children":[
                    {"type":"ask","id":"ask","message":"Choose value",
                     "schema":{
                         "type":"object","properties":{"enabled":{"type":"boolean"}},
                         "required":["enabled"]
                     }},
                    {
                        "type":"condition","id":"answer","pointer":"/ask/enabled",
                        "predicate":{"op":"equals","value":false}
                    }
                ]}}),
            ))
            .await
    }));
    let sender = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(sender) = state.lock().await.remove_pending_user_input(&turn_id) {
                break sender;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    sender
        .send(RequestUserInputResponse {
            answers: std::collections::HashMap::from([(
                "field_0".into(),
                RequestUserInputAnswer {
                    answers: vec!["false".into()],
                },
            )]),
        })
        .map_err(|_| anyhow::anyhow!("input receiver dropped"))?;
    let output = tokio::time::timeout(Duration::from_secs(5), task).await???;
    let snapshot: Value = serde_json::from_str(&output.into_text())?;
    assert_eq!(snapshot["status"]["outcome"], "success", "{snapshot}");
    assert_eq!(snapshot["steps"][0]["detail"]["content"]["enabled"], false);
    assert!(
        state
            .lock()
            .await
            .remove_pending_user_input(&turn_id)
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn native_ask_timeout_closes_pending_input() -> anyhow::Result<()> {
    let (session, turn, tracker) = fixture().await;
    session.services.tool_group_catalog.set_groups_enabled(
        &session.services.tool_group_state,
        [
            crate::tools::groups::WORKFLOWS,
            crate::tools::groups::SESSION,
        ],
        true,
    )?;
    let active = crate::state::ActiveTurn::default();
    let state = active.turn_state.clone();
    *session.active_turn.lock().await = Some(active);
    let turn_id = turn.sub_id.clone();
    let task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        WorkflowHandler
            .handle(invocation(
                session,
                turn,
                tracker,
                "run_workflow",
                json!({"timeout_ms":1000,"tree":{"type":"ask","id":"ask","message":"Confirm"}}),
            ))
            .await
    }));
    // Retain the sender, so the interaction cannot finish on a closed channel.
    let sender = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(sender) = state.lock().await.remove_pending_user_input(&turn_id) {
                break sender;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let output = tokio::time::timeout(Duration::from_secs(5), task).await???;
    let snapshot: Value = serde_json::from_str(&output.into_text())?;
    assert_eq!(snapshot["status"]["outcome"], "timed_out", "{snapshot}");
    assert!(
        sender.is_closed(),
        "terminal run retained a pending interaction"
    );
    Ok(())
}

#[tokio::test]
async fn native_ask_respects_hidden_user_input_and_fallback() -> anyhow::Result<()> {
    let (session, turn, tracker) = fixture().await;
    // This tool is Always by default. Model a host-restricted exposure rather
    // than assuming the ordinary SESSION toggle hides user-input controls.
    session.services.tool_group_catalog.set_tool_exposure(
        "request_user_input",
        mcp_host::prelude::ToolExposure::groups([crate::tools::groups::SESSION]),
    )?;
    session.services.tool_group_catalog.set_groups_enabled(
        &session.services.tool_group_state,
        [crate::tools::groups::WORKFLOWS],
        true,
    )?;
    session.services.tool_group_catalog.set_groups_enabled(
        &session.services.tool_group_state,
        [crate::tools::groups::SESSION],
        false,
    )?;
    let output = WorkflowHandler
        .handle(invocation(
            session,
            turn,
            tracker,
            "run_workflow",
            json!({"tree":{"type":"fallback","id":"root","children":[
                {"type":"ask","id":"ask","message":"Confirm"},
                {"type":"condition","id":"recover","pointer":"/input","predicate":{"op":"exists"}}
            ]}}),
        ))
        .await?;
    let snapshot: Value = serde_json::from_str(&output.into_text())?;
    assert_eq!(snapshot["status"]["outcome"], "success", "{snapshot}");
    assert_eq!(snapshot["steps"][0]["status"]["outcome"], "failure");
    assert!(
        snapshot["steps"][0]["detail"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("no longer available")),
        "{snapshot}"
    );
    Ok(())
}

#[tokio::test]
async fn native_sampling_refuses_clamped_transport_without_provider_calls() -> anyhow::Result<()> {
    let (session, turn, tracker) = fixture().await;
    session.services.model_client.set_clamped(true, None).await;
    session.services.tool_group_catalog.set_groups_enabled(
        &session.services.tool_group_state,
        [crate::tools::groups::WORKFLOWS],
        true,
    )?;
    let output = WorkflowHandler
        .handle(invocation(
            session,
            turn,
            tracker,
            "run_workflow",
            json!({"tree":{"type":"fallback","id":"root","children":[
                {"type":"sample","id":"sample","prompt":"Do not contact any provider"},
                {"type":"condition","id":"recover","pointer":"/input","predicate":{"op":"exists"}}
            ]}}),
        ))
        .await?;
    let snapshot: Value = serde_json::from_str(&output.into_text())?;
    assert_eq!(snapshot["status"]["outcome"], "success", "{snapshot}");
    assert_eq!(
        snapshot["steps"][0]["detail"]["error"],
        "tool-free workflow sampling is unsupported on clamped transports"
    );
    Ok(())
}

#[tokio::test]
async fn native_ask_rechecks_mode_after_waiting_for_dispatch() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    crate::test_support::seed_document(
        home.path(),
        crate::user_documents::DocumentKind::Mode,
        "silent",
        "+++\nid = \"silent\"\ntitle = \"Silent\"\ndescription = \"No interactive input\"\n\
         [capabilities]\nrequest_user_input = false\n+++\nNo interactive input.",
    )
    .await?;
    let (session, turn) = crate::chaos::make_session_and_context_with_home(home.path()).await;
    let (session, turn) = (Arc::new(session), Arc::new(turn));
    let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
    session.services.tool_group_catalog.set_groups_enabled(
        &session.services.tool_group_state,
        [
            crate::tools::groups::WORKFLOWS,
            crate::tools::groups::SESSION,
        ],
        true,
    )?;
    let held = session.services.tool_scheduler.acquire(false).await?;
    let mut execution = Box::pin(WorkflowHandler.handle(invocation(
        session.clone(),
        turn.clone(),
        tracker,
        "run_workflow",
        json!({"tree":{"type":"fallback","id":"root","children":[
            {"type":"ask","id":"ask","message":"Confirm"},
            {"type":"condition","id":"recover","pointer":"/input","predicate":{"op":"exists"}}
        ]}}),
    )));
    assert!(futures::poll!(execution.as_mut()).is_pending());
    // A dispatch task exists, but the leaf cannot yet acquire its permit.
    tokio::task::yield_now().await;
    session
        .switch_mode("silent", &turn)
        .await
        .map_err(anyhow::Error::msg)?;
    drop(held);
    let output = tokio::time::timeout(Duration::from_secs(5), execution).await??;
    let snapshot: Value = serde_json::from_str(&output.into_text())?;
    assert_eq!(snapshot["status"]["outcome"], "success", "{snapshot}");
    assert!(
        snapshot["steps"][0]["detail"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("no longer available")),
        "{snapshot}"
    );
    Ok(())
}

pub(super) fn invocation(
    session: Arc<crate::chaos::Session>,
    turn: Arc<crate::chaos::TurnContext>,
    tracker: crate::tools::context::SharedTurnDiffTracker,
    name: &str,
    args: Value,
) -> ToolInvocation {
    ToolInvocation {
        session,
        turn,
        tracker,
        tool_name: name.into(),
        tool_namespace: None,
        call_id: "test".into(),
        payload: ToolPayload::Function {
            arguments: args.to_string(),
        },
    }
}

#[tokio::test]
async fn condition_workflow_records_steps_and_owned_status() -> anyhow::Result<()> {
    let (session, turn, tracker) = fixture().await;
    session.services.tool_group_catalog.set_groups_enabled(
        &session.services.tool_group_state,
        [crate::tools::groups::WORKFLOWS],
        true,
    )?;
    let result = WorkflowHandler
        .handle(invocation(
            session.clone(),
            turn.clone(),
            tracker.clone(),
            "run_workflow",
            json!({"tree":{
            "type":"condition","id":"check","pointer":"/input/value",
            "predicate":{"op":"equals","value":42}
        },"input":{"value":42}}),
        ))
        .await?;
    let snapshot: Value = serde_json::from_str(&result.into_text())?;
    assert_eq!(
        snapshot["status"],
        json!({"state":"terminal","outcome":"success"})
    );
    assert_eq!(snapshot["steps"][0]["node_id"], "check");
    let id: RunId = serde_json::from_value(snapshot["run_id"].clone())?;
    assert_eq!(
        session.services.workflow_runs.status(id)?.status,
        RunStatus::Terminal(RunOutcome::Success)
    );
    let status = WorkflowHandler
        .handle(invocation(
            session.clone(),
            turn,
            tracker,
            "workflow_status",
            json!({"run_id":id}),
        ))
        .await?;
    let status: Value = serde_json::from_str(&status.into_text())?;
    assert_eq!(status["run_id"], snapshot["run_id"]);
    let (other, turn, tracker) = fixture().await;
    assert!(
        WorkflowHandler
            .handle(invocation(
                other,
                turn,
                tracker,
                "workflow_status",
                json!({"run_id":id})
            ))
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn workflow_group_does_not_gate_owned_status_and_cancel() -> anyhow::Result<()> {
    let (session, turn, _) = fixture().await;
    let cancellation = tokio_util::sync::CancellationToken::new();
    let router = crate::chaos::built_tools(&session, &turn, &[], &cancellation).await?;
    let names: Vec<_> = router
        .model_visible_specs()
        .iter()
        .map(|spec| spec.name().to_string())
        .collect();
    assert!(!names.iter().any(|name| name == "run_workflow"));
    assert!(names.iter().any(|name| name == "workflow_status"));
    assert!(names.iter().any(|name| name == "cancel_workflow"));
    Ok(())
}

#[tokio::test]
async fn owned_cancel_reports_cancelling_until_executor_cleanup() -> anyhow::Result<()> {
    let (session, turn, tracker) = fixture().await;
    let lease = session.services.workflow_runs.start(RunKind::Synopsis)?;
    let token = lease.cancellation();
    let result = WorkflowHandler
        .handle(invocation(
            session.clone(),
            turn,
            tracker,
            "cancel_workflow",
            json!({"run_id":lease.id()}),
        ))
        .await?;
    let snapshot: Value = serde_json::from_str(&result.into_text())?;
    assert_eq!(snapshot["status"], json!({"state":"cancelling"}));
    assert!(token.is_cancelled());
    let snapshot = lease.finish(RunOutcome::Cancelled, None)?;
    assert_eq!(snapshot.status, RunStatus::Terminal(RunOutcome::Cancelled));
    Ok(())
}

#[tokio::test]
async fn invalid_or_hidden_calls_are_rejected_before_run_reservation() -> anyhow::Result<()> {
    let (session, turn, tracker) = fixture().await;
    session.services.tool_group_catalog.set_groups_enabled(
        &session.services.tool_group_state,
        [crate::tools::groups::WORKFLOWS],
        true,
    )?;
    for tree in [
        json!({"type":"call","id":"call","tool":"missing_tool","arguments":{}}),
        json!({"type":"call","id":"call","tool":"functions.run_workflow","arguments":{}}),
        json!({
            "type":"retry","id":"retry","attempts":2,
            "child":{"type":"call","id":"call","tool":"missing_tool","arguments":{}}
        }),
    ] {
        assert!(
            WorkflowHandler
                .handle(invocation(
                    session.clone(),
                    turn.clone(),
                    tracker.clone(),
                    "run_workflow",
                    json!({"tree":tree})
                ))
                .await
                .is_err()
        );
    }
    // Rejections must not occupy any of the four active slots.
    let leases = (0..4)
        .map(|_| session.services.workflow_runs.start(RunKind::Workflow))
        .collect::<Result<Vec<_>, _>>()?;
    drop(leases);
    Ok(())
}

#[tokio::test]
async fn container_dispatch_does_not_take_the_leaf_scheduler_permit() -> anyhow::Result<()> {
    let (session, turn, tracker) = fixture().await;
    let lease = session.services.workflow_runs.start(RunKind::Workflow)?;
    let id = lease.id();
    let router = Arc::new(ToolRouter::from_config(
        &turn.tools_config,
        crate::tools::router::ToolRouterParams {
            mcp_tools: None,
            app_tools: None,
            dynamic_tools: &[],
            catalog_tools: vec![],
            halluacinate: None,
            plan_mode: false,
            tool_groups: None,
        },
    ));
    let held = session.services.tool_scheduler.acquire(false).await?;
    let call = crate::tools::router::ToolCall {
        tool_name: "workflow_status".into(),
        tool_namespace: None,
        call_id: "status".into(),
        payload: ToolPayload::Function {
            arguments: json!({"run_id":id}).to_string(),
        },
    };
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        ToolCallRuntime::new(router, session, turn, tracker)
            .handle_tool_call(call, tokio_util::sync::CancellationToken::new()),
    )
    .await??;
    assert!(matches!(
        result,
        chaos_ipc::models::ResponseInputItem::FunctionCallOutput { .. }
    ));
    drop(held);
    Ok(())
}
