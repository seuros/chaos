use super::*;

#[test]
fn log_message_prefers_human_message_and_preserves_resource_hint() {
    let message = format_log_message(
        "coordinator",
        &LogMessageNotificationParams {
            level: "notice".to_string(),
            logger: Some("coordination.state".to_string()),
            data: serde_json::json!({
                "message": "Coordination state changed; read the inbox.",
                "uri": "agent://inbox",
                "event_id": 42
            }),
        },
    );

    assert_eq!(
        message,
        "MCP coordinator [notice/coordination.state]: Coordination state changed; read the inbox. (agent://inbox)"
    );
}

#[test]
fn log_message_serializes_structured_data_without_message() {
    let message = format_log_message(
        "server",
        &LogMessageNotificationParams {
            level: "info".to_string(),
            logger: None,
            data: serde_json::json!({"ready": true}),
        },
    );

    assert_eq!(message, "MCP server [info]: {\"ready\":true}");
}

#[tokio::test]
async fn resource_update_emits_ui_and_structured_notifications() {
    let (tx_event, rx_event) = async_channel::bounded(1);
    let (notification_tx, notification_rx) = async_channel::bounded(1);

    emit_resource_update(
        &tx_event,
        Some(&notification_tx),
        "coordinator".to_string(),
        "agent://inbox".to_string(),
    )
    .await;

    let event = rx_event.recv().await.expect("UI event");
    assert!(matches!(
        event.msg,
        EventMsg::BackgroundEvent(BackgroundEventEvent { message })
            if message == "MCP coordinator resource updated: agent://inbox"
    ));
    assert_eq!(
        notification_rx
            .recv()
            .await
            .expect("structured notification"),
        McpServerNotification::ResourceUpdated {
            server: "coordinator".to_string(),
            uri: "agent://inbox".to_string(),
        }
    );
}

struct NoopCatalog;

impl McpCatalogSink for NoopCatalog {
    fn register_mcp_tools(&self, _: &str, _: Vec<chaos_traits::catalog::CatalogTool>) {}
    fn register_mcp_resources(
        &self,
        _: &str,
        _: Vec<chaos_traits::catalog::CatalogResource>,
        _: Vec<chaos_traits::catalog::CatalogResourceTemplate>,
    ) {
    }
    fn register_mcp_prompts(&self, _: &str, _: Vec<chaos_traits::catalog::CatalogPrompt>) {}
    fn unregister_mcp(&self, _: &str) {}
    fn unregister_mcp_tools(&self, _: &str) {}
    fn unregister_mcp_resources(&self, _: &str) {}
    fn unregister_mcp_prompts(&self, _: &str) {}
    fn clear_all_mcp(&self) {}
}

fn test_handler(notification_tx: Option<Sender<McpServerNotification>>) -> ChaosClientHandler {
    let (tx_event, _) = async_channel::bounded(1);
    ChaosClientHandler {
        server_name: "peer".into(),
        endpoint: "stdio".into(),
        tx_event,
        notification_tx,
        elicitation_requests: ElicitationRequestManager::new(
            chaos_ipc::protocol::ApprovalPolicy::Interactive,
        ),
        tools_arc: Arc::new(StdRwLock::new(Vec::new())),
        tool_filter: ToolFilter::default(),
        tool_timeout: Duration::from_secs(1),
        session: Arc::new(tokio::sync::RwLock::new(None)),
        catalog: Arc::new(NoopCatalog),
        cwd: Arc::new(StdRwLock::new(PathBuf::from("/tmp"))),
    }
}

#[tokio::test]
async fn fleet_protocol_uses_chaos_names_only() {
    use mcp_guest::ClientHandler;
    let (tx, rx) = async_channel::bounded(1);
    let handler = test_handler(Some(tx));
    handler
        .on_custom_notification(
            FLEET_INBOX_NOTIFICATION.into(),
            Some(serde_json::json!({
                "uri": chaos_mcp_protocol::FLEET_INBOX_URI,
                "message_ids": ["2", "1"]
            })),
        )
        .await;
    assert_eq!(
        rx.try_recv().expect("wake admitted"),
        McpServerNotification::FleetInbox {
            server: "peer".into(),
            uri: chaos_mcp_protocol::FLEET_INBOX_URI.into(),
            message_ids: vec!["1".into(), "2".into()],
        }
    );
    handler
        .on_custom_notification(
            "notifications/skynet/fleet/inbox".into(),
            Some(serde_json::json!({
                "uri": "skynet://fleet/inbox",
                "message_ids": ["1"]
            })),
        )
        .await;
    assert!(rx.try_recv().is_err());

    let info: FleetHostInfo = serde_json::from_value(
        handler
            .on_custom_request(FLEET_HOST_INFO_REQUEST.into(), None)
            .await
            .expect("hostInfo"),
    )
    .expect("shape");
    assert_eq!(info.os, std::env::consts::OS);
    assert!(info.capabilities.is_empty());
    assert!(matches!(
        handler
            .on_custom_request("skynet/fleet/hostInfo".into(), None)
            .await,
        Err(mcp_guest::GuestError::MethodNotSupported(method))
            if method == "skynet/fleet/hostInfo"
    ));
}
