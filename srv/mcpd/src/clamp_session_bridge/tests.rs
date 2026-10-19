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
    let state = Arc::new(BridgeState::new(
        PathBuf::from("/unused"),
        "token".to_string(),
        registry.clone(),
    ));

    state
        .replace_visible_tools(vec![tool_spec("enable_tools")])
        .expect("initial tool list");
    assert!(state.is_visible("enable_tools"));
    assert!(!state.is_visible("exec_command"));

    state
        .replace_visible_tools(vec![tool_spec("exec_command")])
        .expect("refreshed tool list");
    assert!(!state.is_visible("enable_tools"));
    assert!(state.is_visible("exec_command"));
    assert!(registry.get("enable_tools").is_none());
    assert!(registry.get("exec_command").is_some());
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

    let response = response_input_to_tool_output(output, false, None)
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

    let response = response_input_to_tool_output(output, false, None)
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

#[test]
fn runtime_notices_ride_on_structured_and_content_results() {
    let structured =
        ToolOutput::structured(serde_json::json!({ "output": "ok" })).expect("structured output");
    let with_notice = with_runtime_notices(structured, vec!["post a line".to_string()])
        .expect("structured notice");
    assert_eq!(
        with_notice.into_response_value()["structuredContent"]["runtime_notice"],
        serde_json::json!("post a line")
    );

    let content = with_runtime_notices(
        ToolOutput::text("done"),
        vec!["first".to_string(), "second".to_string()],
    )
    .expect("content notices");
    let value = content.into_response_value();
    assert_eq!(value["content"][0]["text"], serde_json::json!("done"));
    assert_eq!(
        value["content"][1]["text"],
        serde_json::json!("[runtime notice]\nfirst\n\nsecond")
    );

    let untouched =
        with_runtime_notices(ToolOutput::text("plain"), Vec::new()).expect("unchanged content");
    assert_eq!(
        untouched.into_response_value()["content"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
}

#[test]
fn runtime_notices_are_declared_without_weakening_the_native_result_schema() {
    let mut spec = tool_spec("typed");
    spec.output_schema = Some(serde_json::json!({
        "type": "object",
        "properties": {"output": {"type": "string"}},
        "required": ["output"],
        "additionalProperties": false
    }));
    let state = Arc::new(BridgeState::new(
        PathBuf::from("/unused"),
        "token".into(),
        ToolRegistry::new(),
    ));
    let tool = BridgeTool::new(state, spec);
    let schema = tool.output_schema().expect("bridge schema");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"], serde_json::json!(["output"]));
    assert_eq!(
        schema["properties"]["runtime_notice"],
        serde_json::json!({"type":"string"})
    );
    let output = with_runtime_notices(
        ToolOutput::structured(serde_json::json!({"output":"value"})).expect("typed value"),
        vec!["host notice".into()],
    )
    .expect("notice");
    assert!(tool.validate_output(&output).is_ok());
    let value = output.into_response_value();
    for key in value["structuredContent"]
        .as_object()
        .expect("object")
        .keys()
    {
        assert!(schema["properties"].get(key).is_some());
    }
}

#[test]
fn runtime_notice_collision_never_overwrites_tool_data() {
    let output = ToolOutput::structured(serde_json::json!({
        "runtime_notice": {"original":"tool data"}
    }))
    .expect("typed result");
    assert!(matches!(
        with_runtime_notices(output, vec!["host notice".into()]),
        Err(ToolError::Internal(_))
    ));
}

fn native_text_result(text: &str, success: bool) -> ResponseInputItem {
    ResponseInputItem::FunctionCallOutput {
        call_id: "native".into(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(text.into()),
            success: Some(success),
        },
        tool_name: None,
    }
}

#[test]
fn native_typed_output_is_not_reconstructed_from_terminal_text() {
    let typed = serde_json::json!({
        "wall_time_seconds": 1.25,
        "output": "hello",
        "exit_code": 0,
    });
    let output = response_input_to_tool_output(
        native_text_result("readable terminal report", true),
        true,
        Some(typed.clone()),
    )
    .expect("typed native result");
    assert_eq!(output.into_response_value()["structuredContent"], typed);
}

#[test]
fn native_json_results_keep_their_actual_object_shape() {
    let value = serde_json::json!({"agent_id":"child", "status":"running"});
    let output =
        response_input_to_tool_output(native_text_result(&value.to_string(), true), true, None)
            .expect("JSON native result");
    assert_eq!(output.into_response_value()["structuredContent"], value);
}

#[test]
fn a_json_text_block_does_not_hide_an_accompanying_image() {
    let output = ResponseInputItem::FunctionCallOutput {
        call_id: "mixed-json".into(),
        output: FunctionCallOutputPayload::from_content_items(vec![
            FunctionCallOutputContentItem::InputText {
                text: r#"{"preview":"image"}"#.into(),
            },
            FunctionCallOutputContentItem::InputImage {
                image_url: "data:image/png;base64,Zm9v".into(),
                detail: None,
            },
        ]),
        tool_name: None,
    };
    let output = response_input_to_tool_output(output, true, None)
        .expect("mixed content stays available for SDK validation");
    assert!(output.is_content());
    let response = output.into_response_value();
    assert_eq!(response["content"].as_array().map(Vec::len), Some(2));
    assert_eq!(response["content"][1]["type"], "image");
}

#[test]
fn missing_typed_output_is_not_hidden_by_a_fabricated_wrapper() {
    let output = response_input_to_tool_output(
        native_text_result("terminal output without typed timing", true),
        true,
        None,
    )
    .expect("content is left for SDK schema validation");
    assert!(output.is_content());
    let mut spec = tool_spec("exec_command");
    spec.output_schema = Some(serde_json::json!({
        "type": "object",
        "properties": {
            "wall_time_seconds": {"type": "number"},
            "output": {"type": "string"}
        },
        "required": ["wall_time_seconds", "output"],
        "additionalProperties": false
    }));
    let state = Arc::new(BridgeState::new(
        PathBuf::from("/unused"),
        "token".into(),
        ToolRegistry::new(),
    ));
    assert!(
        BridgeTool::new(state, spec)
            .validate_output(&output)
            .is_err()
    );
}

#[test]
fn failed_native_and_mcp_results_remain_execution_errors() {
    let native =
        response_input_to_tool_output(native_text_result("native denied", false), true, None);
    assert!(matches!(native, Err(ToolError::Execution(message)) if message == "native denied"));
    let mcp = response_input_to_tool_output(
        ResponseInputItem::McpToolCallOutput {
            call_id: "mcp-error".into(),
            output: chaos_ipc::mcp::CallToolResult::from_error_text("MCP denied".into()),
        },
        true,
        None,
    );
    assert!(matches!(mcp, Err(ToolError::Execution(message)) if message == "MCP denied"));
}

#[test]
fn mcp_results_match_the_declared_call_result_envelope() {
    let result = chaos_ipc::mcp::CallToolResult {
        content: vec![serde_json::json!({"type":"text","text":"answer"})],
        structured_content: Some(serde_json::json!({"answer":42})),
        is_error: Some(false),
        meta: Some(serde_json::json!({"provenance":"driver"})),
    };
    let expected = serde_json::to_value(&result).expect("call result JSON");
    let output = response_input_to_tool_output(
        ResponseInputItem::McpToolCallOutput {
            call_id: "mcp-success".into(),
            output: result,
        },
        true,
        None,
    )
    .expect("MCP call result");
    assert_eq!(output.into_response_value()["structuredContent"], expected);
}
