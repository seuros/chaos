use super::*;
use crate::client_common::Prompt;
use crate::client_common::tools::FreeformToolFormat;
use chaos_parrot::sanitize::JsonSchema;
use serde_json::json;

#[cfg(unix)]
mod antigravity_acp;

/// Exercise the real session-backed Unix bridge and kernel read tool without a
/// provider. This is not yet the combined AGY ACP -> MCP -> kernel acceptance.
#[tokio::test]
async fn session_bridge_reads_fixture_through_kernel_tool_dispatch() {
    let (_home, workspace, session) = session_bridge_fixture().await;
    let file = workspace.path().join("kernel-bridge-fixture.txt");
    let marker = uuid::Uuid::now_v7().to_string();
    std::fs::write(&file, &marker).expect("write fixture");
    let bridge = ClampSessionBridge::spawn(Arc::downgrade(&session))
        .await
        .expect("bridge");
    let listed = bridge_test_request(
        bridge.socket_path(),
        ClampBridgeRequest::ListTools {
            token: bridge.token().to_string(),
        },
    )
    .await;
    let result = bridge_test_request(
        bridge.socket_path(),
        ClampBridgeRequest::CallTool {
            token: bridge.token().to_string(),
            name: "read_file".into(),
            arguments: json!({"file_path": file, "offset": 1, "limit": 5}),
        },
    )
    .await;
    bridge.shutdown().await.expect("stop bridge");
    let tools = match listed.expect("list response") {
        ClampBridgeResponse::Tools { tools } => tools,
        _ => panic!("bridge did not list tools"),
    };
    assert!(tools.iter().any(|tool| tool.name == "read_file"));
    let result = result.expect("read response");
    assert!(
        matches!(result, ClampBridgeResponse::ToolResult { .. }),
        "kernel read did not return a tool result"
    );
    assert!(
        serde_json::to_string(&result)
            .expect("result JSON")
            .contains(&marker)
    );
}

/// A plan attached after the turn started (by the operator CLI or a previous
/// tool call) must reach the clamped model's tool list, as it reaches the
/// prompt. The task's turn context is the one captured at turn start.
#[tokio::test]
async fn session_bridge_lists_plan_progress_for_plan_attached_mid_turn() {
    use chaos_proc::planning::{PlanChange, PlanMutation, PlanningActor};
    use chaos_proc::{RuntimeDbHandle, StateRuntime};

    let (home, _workspace, session) = session_bridge_fixture().await;
    let db = RuntimeDbHandle::Sqlite(
        StateRuntime::init(home.path().into(), "test".into())
            .await
            .expect("runtime db"),
    );
    let workspace = db
        .planning_create_workspace("bridge")
        .await
        .expect("workspace");
    let plan = db
        .planning_mutate(
            &PlanningActor {
                session: "operator".into(),
                installation: "test".into(),
            },
            &PlanMutation {
                request_id: "bridge-plan".into(),
                plan: None,
                expected_revision: None,
                change: PlanChange::Create {
                    workspace: workspace.id.clone(),
                    title: "Bridge".into(),
                    body: String::new(),
                },
            },
        )
        .await
        .expect("plan")
        .plan;
    let session = {
        let mut session = Arc::into_inner(session).expect("sole session owner");
        session.services.runtime_db = Some(db.clone());
        Arc::new(session)
    };
    let bridge = ClampSessionBridge::spawn(Arc::downgrade(&session))
        .await
        .expect("bridge");
    let list = || async {
        match bridge_test_request(
            bridge.socket_path(),
            ClampBridgeRequest::ListTools {
                token: bridge.token().to_string(),
            },
        )
        .await
        .expect("list response")
        {
            ClampBridgeResponse::Tools { tools } => {
                tools.into_iter().map(|tool| tool.name).collect::<Vec<_>>()
            }
            _ => panic!("bridge did not list tools"),
        }
    };

    assert!(!list().await.iter().any(|name| name == "plan_progress"));
    db.planning_attach(&session.conversation_id.to_string(), Some(&plan.id))
        .await
        .expect("attach");
    assert!(
        list().await.iter().any(|name| name == "plan_progress"),
        "a plan attached mid-turn is missing from the clamp tool list"
    );
    db.planning_attach(&session.conversation_id.to_string(), None)
        .await
        .expect("detach");
    assert!(!list().await.iter().any(|name| name == "plan_progress"));
    bridge.shutdown().await.expect("stop bridge");
}

async fn session_bridge_fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    Arc<crate::chaos::Session>,
) {
    use crate::state::{ActiveTurn, RunningTask, TaskKind};
    use tokio::sync::Notify;
    use tokio_util::task::AbortOnDropHandle;

    let home = tempfile::tempdir().expect("isolated Chaos home");
    let workspace = tempfile::tempdir().expect("isolated workspace");
    let (session, mut turn) = crate::chaos::make_session_and_context_with_home(home.path()).await;
    turn.cwd = workspace.path().to_path_buf();
    let session = Arc::new(session);
    session
        .services
        .tool_group_catalog
        .set_groups_enabled(
            &session.services.tool_group_state,
            [crate::tools::groups::FILESYSTEM.to_string()],
            true,
        )
        .expect("enable fixture filesystem tools");
    let mut active = ActiveTurn::default();
    active.add_task(RunningTask {
        done: Arc::new(Notify::new()),
        kind: TaskKind::Regular,
        task: Arc::new(crate::tasks::RegularTask::default()),
        cancellation_token: CancellationToken::new(),
        handle: AbortOnDropHandle::new(tokio::spawn(std::future::pending())),
        turn_context: Arc::new(turn),
        unrecorded_input: Arc::default(),
        _timer: None,
    });
    *session.active_turn.lock().await = Some(active);
    (home, workspace, session)
}

async fn bridge_test_request(
    path: &Path,
    request: ClampBridgeRequest,
) -> io::Result<ClampBridgeResponse> {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let mut stream = UnixStream::connect(path).await?;
        let mut request = serde_json::to_vec(&request)?;
        request.push(b'\n');
        stream.write_all(&request).await?;
        let mut line = String::new();
        tokio::io::BufReader::new(stream)
            .read_line(&mut line)
            .await?;
        Ok(serde_json::from_str(&line)?)
    })
    .await
    .map_err(io::Error::other)?
}

#[test]
fn antigravity_catalogue_projects_current_mcp_tools_not_native_tools() {
    let parameters = JsonSchema::Object {
        properties: Default::default(),
        required: None,
        additional_properties: None,
    };
    let mut prompt = Prompt::default();
    prompt.base_instructions.text = "Canonical instructions".into();
    prompt.tools = vec![
        ToolSpec::Function(ResponsesApiTool {
            name: "lookup".into(),
            description: "Read data".into(),
            strict: false,
            defer_loading: None,
            parameters: parameters.clone(),
            output_schema: Some(json!({"type": "string"})),
        }),
        ToolSpec::Freeform(FreeformTool {
            name: "patch".into(),
            description: "Apply changes".into(),
            format: FreeformToolFormat {
                r#type: "grammar".into(),
                syntax: "lark".into(),
                definition: "unused".into(),
            },
        }),
        ToolSpec::LocalShell {},
        ToolSpec::ImageGeneration {
            output_format: "png".into(),
        },
        ToolSpec::WebSearch {
            external_web_access: None,
            filters: None,
            user_location: None,
            search_context_size: None,
            search_content_types: None,
        },
        ToolSpec::ToolSearch {
            execution: "server".into(),
            description: "Native search".into(),
            parameters: parameters.clone(),
        },
    ];
    let rendered = antigravity_system_prompt(&prompt);
    assert!(rendered.starts_with("Canonical instructions\n\n"));
    assert!(rendered.contains("ServerName \"chaos\""));
    let tools: serde_json::Value =
        serde_json::from_str(rendered.rsplit_once("\n\n").unwrap().1).unwrap();
    assert_eq!(tools.as_array().unwrap().len(), 2);
    assert_eq!(tools[0]["name"], "lookup");
    assert_eq!(
        tools[0]["inputSchema"],
        serde_json::Value::from(&parameters)
    );
    assert_eq!(tools[0]["outputSchema"], json!({"type": "string"}));
    assert_eq!(tools[1]["name"], "patch");
    assert_eq!(
        tools[1]["inputSchema"],
        json!({
            "type": "object", "properties": {"input": {
                "type": "string", "description": "Freeform grammar input (syntax: lark)."
            }}, "required": ["input"], "additionalProperties": false
        })
    );
    // The next sampling turn must not advertise a removed tool.
    prompt.tools.remove(0);
    let next = antigravity_system_prompt(&prompt);
    assert!(!next.contains("lookup"));
    assert!(next.contains("patch"));
    prompt.tools.clear();
    prompt.base_instructions.text = "Changed instructions".into();
    let empty = antigravity_system_prompt(&prompt);
    assert!(empty.starts_with("Changed instructions\n\n"));
    assert!(empty.ends_with("[]"));
}

#[test]
fn steered_user_text_becomes_notices_and_other_items_stay_pending() {
    use chaos_ipc::models::ContentItem;
    use chaos_ipc::models::ResponseInputItem;

    let steer = ResponseInputItem::Message {
        role: "user".into(),
        content: vec![ContentItem::InputText {
            text: "No update for 2m; post a line.".into(),
        }],
    };
    let developer = ResponseInputItem::Message {
        role: "developer".into(),
        content: vec![ContentItem::InputText {
            text: "kernel note".into(),
        }],
    };

    let (notices, other) = split_steered_notices(vec![steer, developer.clone()]);

    assert_eq!(notices, vec!["No update for 2m; post a line.".to_string()]);
    assert_eq!(other, vec![developer]);
}

#[tokio::test]
async fn antigravity_tool_image_staging_handle_rejects_late_stage_after_proxy_replacement() {
    use chaos_ipc::models::{
        FunctionCallOutputBody, FunctionCallOutputContentItem, FunctionCallOutputPayload,
    };

    struct NoopSink;
    impl chaos_clamp::WiretapSink for NoopSink {
        fn record(&self, _exchange: chaos_clamp::WiretapExchange) {}
    }

    let (home, _workspace, session) = session_bridge_fixture().await;
    session
        .services
        .model_client
        .set_clamped(true, Some(crate::config::ClampBackend::Antigravity))
        .await;

    let start_proxy = |label: &str| {
        let ca_path = home.path().join(format!("egress-ca-{label}.pem"));
        let policy = chaos_clamp::EgressPolicy::antigravity()
            .with_antigravity_system_prompt(format!("instructions {label}"));
        chaos_clamp::EgressProxy::start(policy, Arc::new(NoopSink), Some(ca_path))
    };

    // Start proxy #1 and arm turn 1 (generation 1).
    let proxy1 = start_proxy("one").await.expect("start proxy 1");
    let guard1 = proxy1
        .arm_antigravity_tool_images("gemini-4-neon-high".to_string())
        .expect("arm proxy 1");
    assert_eq!(guard1.generation_id(), 1);
    session
        .services
        .model_client
        .replace_antigravity_egress_for_test(Some(proxy1))
        .await;

    // Capture the turn context + staging handle at bridge request start.
    let (_turn_context, handle1) = active_clamp_turn_and_tool_image_handle(&session, true)
        .await
        .expect("capture handle 1")
        .expect("active turn");
    let handle1 = handle1.expect("armed handle 1");
    assert_eq!(handle1.generation_id(), 1);

    // Replace the session's egress proxy with proxy #2 and arm it (also generation 1).
    drop(guard1);
    let proxy2 = start_proxy("two").await.expect("start proxy 2");
    let guard2 = proxy2
        .arm_antigravity_tool_images("gemini-4-neon-high".to_string())
        .expect("arm proxy 2");
    assert_eq!(guard2.generation_id(), 1);
    if let Some(old_proxy) = session
        .services
        .model_client
        .replace_antigravity_egress_for_test(Some(proxy2))
        .await
    {
        old_proxy.shutdown();
    }

    // Late tool completion from turn 1 must fail closed rather than staging into proxy #2.
    let mut late_output = ResponseInputItem::FunctionCallOutput {
        call_id: "call_late".to_string(),
        tool_name: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::ContentItems(vec![
                FunctionCallOutputContentItem::InputImage {
                    image_url: "data:image/png;base64,iVBORw0KGgo=".to_string(),
                    detail: None,
                },
            ]),
            success: Some(true),
        },
    };
    let err = stage_antigravity_tool_images(&session, Some(&handle1), &mut late_output)
        .expect_err("late staging against replaced proxy must fail");
    assert!(
        err.contains("no longer active") || err.contains("no active Antigravity turn"),
        "unexpected error: {err}"
    );
    // Output must not have been rewritten into a nonce marker on failure.
    assert!(matches!(
        late_output,
        ResponseInputItem::FunctionCallOutput {
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::ContentItems(ref items),
                ..
            },
            ..
        } if matches!(items.as_slice(), [FunctionCallOutputContentItem::InputImage { .. }])
    ));

    // A fresh bridge call on proxy #2 captures a new handle and stages cleanly.
    let (_turn_context, handle2) = active_clamp_turn_and_tool_image_handle(&session, true)
        .await
        .expect("capture handle 2")
        .expect("active turn");
    let handle2 = handle2.expect("armed handle 2");
    assert_eq!(handle2.generation_id(), 1);

    let mut fresh_output = ResponseInputItem::FunctionCallOutput {
        call_id: "call_fresh".to_string(),
        tool_name: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::ContentItems(vec![
                FunctionCallOutputContentItem::InputImage {
                    image_url: "data:image/png;base64,iVBORw0KGgo=".to_string(),
                    detail: None,
                },
            ]),
            success: Some(true),
        },
    };
    stage_antigravity_tool_images(&session, Some(&handle2), &mut fresh_output)
        .expect("fresh handle on replacement proxy should stage");
    match fresh_output {
        ResponseInputItem::FunctionCallOutput {
            output:
                FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::ContentItems(items),
                    ..
                },
            ..
        } => match items.as_slice() {
            [FunctionCallOutputContentItem::InputText { text }] => {
                assert!(text.starts_with("[chaos-image id "));
            }
            other => panic!("expected rewritten marker, got {other:?}"),
        },
        other => panic!("unexpected output: {other:?}"),
    }

    drop(guard2);
    if let Some(proxy) = session
        .services
        .model_client
        .replace_antigravity_egress_for_test(None)
        .await
    {
        proxy.shutdown();
    }
}
