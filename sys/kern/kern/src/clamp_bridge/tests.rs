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
