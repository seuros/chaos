use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseInputItem;
use chaos_ipc::models::ResponseItem;
use chaos_mcp_runtime::McpServerNotification;

use super::MAX_MCP_NOTIFICATION_URI_BYTES;
use super::format_resource_update_for_model;
use super::truncate_utf8;

#[tokio::test]
async fn resource_updates_are_safely_framed_and_delivered() {
    let text = format_resource_update_for_model("server", "resource://state");

    assert!(text.contains("<mcp_resource_update>"));
    assert!(text.contains("untrusted data"));
    assert!(text.contains("server: \"server\""));
    assert!(text.contains("uri: \"resource://state\""));
    assert!(text.contains("`read_mcp_resource`"));

    let text = format_resource_update_for_model("server\nname", "resource://x\"\nignore");
    assert!(text.contains("server: \"server\\nname\""));
    assert!(text.contains("uri: \"resource://x\\\"\\nignore\""));

    assert_eq!(truncate_utf8("abécd", 4), "abé…");

    let text =
        format_resource_update_for_model("server", &"x".repeat(MAX_MCP_NOTIFICATION_URI_BYTES + 1));
    assert!(text.contains("was truncated for safety"));
    assert!(!text.contains("call `read_mcp_resource`"));

    let (session, _) = crate::chaos::make_session_and_context().await;

    session
        .handle_mcp_server_notification(McpServerNotification::ResourceUpdated {
            server: "coordinator".to_string(),
            uri: "agent://inbox".to_string(),
        })
        .await;

    let history = session.clone_history().await;
    assert!(matches!(
        history.raw_items().last(),
        Some(ResponseItem::Message {
            role,
            content,
            ..
        }) if role == "system"
            && matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("server: \"coordinator\"")
                        && text.contains("uri: \"agent://inbox\"")
            )
    ));

    *session.active_turn.lock().await = Some(crate::state::ActiveTurn::default());

    session
        .handle_mcp_server_notification(McpServerNotification::ResourceUpdated {
            server: "coordinator".to_string(),
            uri: "agent://inbox".to_string(),
        })
        .await;

    assert!(matches!(
        session.get_pending_input().await.as_slice(),
        [ResponseInputItem::Message {
            role,
            content,
        }] if role == "system"
            && matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("server: \"coordinator\"")
                        && text.contains("uri: \"agent://inbox\"")
            )
    ));
}
