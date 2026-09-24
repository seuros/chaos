use super::*;
use crate::tools::groups;

async fn receive_tool_summaries(receiver: &async_channel::Receiver<Event>) -> Vec<ToolSummary> {
    let event = receiver.recv().await.expect("all-tools response");
    let EventMsg::AllToolsResponse(response) = event.msg else {
        panic!("expected all-tools response");
    };
    response.tools
}

#[test]
fn qualified_mcp_tools_keep_their_server_without_catching_mcp_helpers() {
    let catalog = Catalog::from_inventory();
    let mcp_tools = HashMap::from([(
        "mcp__example__inspect".to_string(),
        McpToolPresentation {
            server_name: "example".to_string(),
            display_name: "inspect".to_string(),
            catalog_tool: chaos_traits::catalog::CatalogTool {
                name: "inspect".to_string(),
                description: String::new(),
                input_schema: serde_json::json!({}),
                annotations: Some(serde_json::json!({
                    "readOnlyHint": true,
                    "openWorldHint": false
                })),
                read_only_hint: Some(true),
                supports_parallel_tool_calls: true,
            },
        },
    )]);
    let dynamic_tool_names = HashSet::new();
    let script_tool_names = HashSet::new();

    let presentation = tool_presentation(
        "mcp__example__inspect",
        &catalog,
        &mcp_tools,
        &dynamic_tool_names,
        &script_tool_names,
    );
    assert_eq!(presentation.display_name, "inspect");
    assert_eq!(presentation.source, "mcp:example");
    assert!(
        presentation
            .annotation_labels
            .iter()
            .any(|label| label == "read-only")
    );
    assert!(presentation.annotations.is_some());

    let presentation = tool_presentation(
        "list_mcp_resources",
        &catalog,
        &mcp_tools,
        &dynamic_tool_names,
        &script_tool_names,
    );
    assert_eq!(presentation.display_name, "list_mcp_resources");
    assert_eq!(presentation.source, "builtin");
}

#[tokio::test]
async fn listing_uses_model_visible_registry_for_native_and_grouped_tools() {
    let (session, turn_context, receiver) = crate::chaos::make_session_and_context_with_rx().await;

    list_all_tools(
        &session,
        &turn_context.config,
        "list-default-tools".to_string(),
    )
    .await;
    let tools = receive_tool_summaries(&receiver).await;
    let names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();

    assert!(names.contains("enable_tools"));
    assert!(names.contains("switch_mode"));
    assert!(names.contains("request_user_input"));
    assert!(!names.contains("disable_tools"));
    assert!(!names.contains("cron_create"));
    assert!(!names.contains("read_file"));
    assert_eq!(
        tools
            .iter()
            .find(|tool| tool.name == "enable_tools")
            .map(|tool| tool.source.as_str()),
        Some("builtin")
    );

    session
        .services
        .tool_group_catalog
        .set_groups_enabled(&session.services.tool_group_state, [groups::CRON], true)
        .expect("enable cron tools");
    list_all_tools(
        &session,
        &turn_context.config,
        "list-enabled-tools".to_string(),
    )
    .await;
    let tools = receive_tool_summaries(&receiver).await;
    let names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();

    assert!(names.contains("enable_tools"));
    assert!(names.contains("disable_tools"));
    assert!(names.contains("cron_create"));
    assert_eq!(
        tools
            .iter()
            .find(|tool| tool.name == "cron_create")
            .map(|tool| tool.source.as_str()),
        Some("cron")
    );
}
