use super::*;
use chaos_ipc::models::FunctionCallOutputPayload;

fn tool_spec(name: &str) -> BridgeToolSpec {
    BridgeToolSpec {
        name: name.to_string(),
        title: None,
        description: None,
        input_schema: serde_json::json!({ "type": "object" }),
        output_schema: None,
        annotations: None,
        icons: None,
        meta: None,
    }
}

#[test]
fn clamp_bridge_replaces_visible_tools_after_group_changes() {
    let registry = ToolRegistry::new();
    let (notification_sender, mut notification_rx) = NotificationSender::bounded(4);
    let state = Arc::new(BridgeState::new(
        PathBuf::from("/unused"),
        "token".to_string(),
        registry.clone(),
        notification_sender,
    ));

    state
        .replace_visible_tools(vec![tool_spec("enable_tools")], false)
        .expect("initial tool list");
    assert!(state.is_visible("enable_tools"));
    assert!(!state.is_visible("exec_command"));
    assert!(notification_rx.try_recv().is_err());

    state
        .replace_visible_tools(vec![tool_spec("exec_command")], true)
        .expect("refreshed tool list");
    assert!(!state.is_visible("enable_tools"));
    assert!(state.is_visible("exec_command"));
    assert!(registry.get("enable_tools").is_some());
    assert!(registry.get("exec_command").is_some());

    let notification = notification_rx
        .try_recv()
        .expect("tool list changed notification");
    assert_eq!(notification.method, "notifications/tools/list_changed");
}

#[test]
fn clamp_bridge_preserves_image_only_function_output() {
    let output = ResponseInputItem::FunctionCallOutput {
        call_id: "view-image".to_string(),
        output: FunctionCallOutputPayload::from_content_items(vec![
            FunctionCallOutputContentItem::InputImage {
                image_url: "data:image/png;base64,Zm9v".to_string(),
                detail: None,
            },
        ]),
        tool_name: Some("view_image".to_string()),
    };

    let response = response_input_to_tool_output(output, false)
        .expect("image output should convert")
        .into_response_value();

    assert_eq!(
        response,
        serde_json::json!({
            "content": [{
                "type": "image",
                "data": "Zm9v",
                "mimeType": "image/png"
            }]
        })
    );
}

#[test]
fn clamp_bridge_preserves_mixed_text_and_image_function_output() {
    let output = ResponseInputItem::FunctionCallOutput {
        call_id: "mixed-output".to_string(),
        output: FunctionCallOutputPayload::from_content_items(vec![
            FunctionCallOutputContentItem::InputText {
                text: "preview".to_string(),
            },
            FunctionCallOutputContentItem::InputImage {
                image_url: "data:image/webp;base64,Zm9v".to_string(),
                detail: None,
            },
        ]),
        tool_name: None,
    };

    let response = response_input_to_tool_output(output, false)
        .expect("mixed output should convert")
        .into_response_value();

    assert_eq!(
        response,
        serde_json::json!({
            "content": [
                {
                    "type": "text",
                    "text": "preview"
                },
                {
                    "type": "image",
                    "data": "Zm9v",
                    "mimeType": "image/webp"
                }
            ]
        })
    );
}
