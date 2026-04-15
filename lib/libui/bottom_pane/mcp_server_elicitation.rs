mod domain;
mod formatting;
mod parsing;
mod support;
mod ui;

pub use domain::McpServerElicitationFormRequest;
pub use domain::ToolSuggestionType;
pub use ui::McpServerElicitationOverlay;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use crate::test_render::render_to_first_char_string;
    use crate::test_support::make_app_event_sender_with_rx;
    use chaos_ipc::ProcessId;
    use chaos_ipc::approvals::ElicitationAction;
    use chaos_ipc::approvals::ElicitationRequest;
    use chaos_ipc::approvals::ElicitationRequestEvent;
    use chaos_ipc::mcp::RequestId as McpRequestId;
    use chaos_ipc::protocol::Op;
    use domain::{
        APPROVAL_ACCEPT_ONCE_VALUE, APPROVAL_CANCEL_VALUE, APPROVAL_DECLINE_VALUE,
        APPROVAL_FIELD_ID, APPROVAL_META_KIND_KEY, APPROVAL_META_KIND_MCP_TOOL_CALL,
        APPROVAL_PERSIST_ALWAYS_VALUE, APPROVAL_PERSIST_KEY, APPROVAL_PERSIST_SESSION_VALUE,
        APPROVAL_TOOL_PARAMS_DISPLAY_KEY, APPROVAL_TOOL_PARAMS_KEY, McpServerElicitationField,
        McpServerElicitationFieldInput, McpServerElicitationOption,
        McpServerElicitationResponseMode, McpToolApprovalDisplayParam, ToolSuggestionRequest,
        ToolSuggestionToolType,
    };
    use pretty_assertions::assert_eq;
    use ratatui::layout::Rect;
    use serde_json::Value;

    fn test_sender() -> (
        crate::app_event_sender::AppEventSender,
        tokio::sync::mpsc::UnboundedReceiver<crate::app_event::AppEvent>,
    ) {
        make_app_event_sender_with_rx()
    }

    fn form_request(
        message: &str,
        requested_schema: Value,
        meta: Option<Value>,
    ) -> ElicitationRequestEvent {
        ElicitationRequestEvent {
            turn_id: Some("turn-1".to_string()),
            server_name: "server-1".to_string(),
            id: McpRequestId::String("request-1".to_string()),
            request: ElicitationRequest::Form {
                meta,
                message: message.to_string(),
                requested_schema,
            },
        }
    }

    fn empty_object_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
        })
    }

    fn tool_approval_meta(
        persist_modes: &[&str],
        tool_params: Option<Value>,
        tool_params_display: Option<Vec<(&str, Value, &str)>>,
    ) -> Option<Value> {
        let mut meta = serde_json::Map::from_iter([(
            APPROVAL_META_KIND_KEY.to_string(),
            Value::String(APPROVAL_META_KIND_MCP_TOOL_CALL.to_string()),
        )]);
        if !persist_modes.is_empty() {
            meta.insert(
                APPROVAL_PERSIST_KEY.to_string(),
                Value::Array(
                    persist_modes
                        .iter()
                        .map(|mode| Value::String((*mode).to_string()))
                        .collect(),
                ),
            );
        }
        if let Some(tool_params) = tool_params {
            meta.insert(APPROVAL_TOOL_PARAMS_KEY.to_string(), tool_params);
        }
        if let Some(tool_params_display) = tool_params_display {
            meta.insert(
                APPROVAL_TOOL_PARAMS_DISPLAY_KEY.to_string(),
                Value::Array(
                    tool_params_display
                        .into_iter()
                        .map(|(name, value, display_name)| {
                            serde_json::json!({
                                "name": name,
                                "value": value,
                                "display_name": display_name,
                            })
                        })
                        .collect(),
                ),
            );
        }
        Some(Value::Object(meta))
    }

    #[test]
    fn parses_boolean_form_request() {
        let process_id = ProcessId::default();
        let request = McpServerElicitationFormRequest::from_event(
            process_id,
            form_request(
                "Allow this request?",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "confirmed": {
                            "type": "boolean",
                            "title": "Confirm",
                            "description": "Approve the pending action.",
                        }
                    },
                    "required": ["confirmed"],
                }),
                None,
            ),
        )
        .expect("expected supported form");

        assert_eq!(
            request,
            McpServerElicitationFormRequest {
                process_id,
                server_name: "server-1".to_string(),
                request_id: McpRequestId::String("request-1".to_string()),
                message: "Allow this request?".to_string(),
                approval_display_params: Vec::new(),
                response_mode: McpServerElicitationResponseMode::FormContent,
                fields: vec![McpServerElicitationField {
                    id: "confirmed".to_string(),
                    label: "Confirm".to_string(),
                    prompt: "Approve the pending action.".to_string(),
                    required: true,
                    input: McpServerElicitationFieldInput::Select {
                        options: vec![
                            McpServerElicitationOption {
                                label: "True".to_string(),
                                description: None,
                                value: Value::Bool(true),
                            },
                            McpServerElicitationOption {
                                label: "False".to_string(),
                                description: None,
                                value: Value::Bool(false),
                            },
                        ],
                        default_idx: None,
                    },
                }],
                tool_suggestion: None,
            }
        );
    }

    #[test]
    fn unsupported_numeric_form_falls_back() {
        let request = McpServerElicitationFormRequest::from_event(
            ProcessId::default(),
            form_request(
                "Pick a number",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "count": {
                            "type": "integer",
                            "title": "Count",
                        }
                    },
                }),
                None,
            ),
        );

        assert_eq!(request, None);
    }

    #[test]
    fn missing_schema_uses_approval_actions() {
        let process_id = ProcessId::default();
        let request = McpServerElicitationFormRequest::from_event(
            process_id,
            form_request("Allow this request?", Value::Null, None),
        )
        .expect("expected approval fallback");

        assert_eq!(
            request,
            McpServerElicitationFormRequest {
                process_id,
                server_name: "server-1".to_string(),
                request_id: McpRequestId::String("request-1".to_string()),
                message: "Allow this request?".to_string(),
                approval_display_params: Vec::new(),
                response_mode: McpServerElicitationResponseMode::ApprovalAction,
                fields: vec![McpServerElicitationField {
                    id: APPROVAL_FIELD_ID.to_string(),
                    label: String::new(),
                    prompt: String::new(),
                    required: true,
                    input: McpServerElicitationFieldInput::Select {
                        options: vec![
                            McpServerElicitationOption {
                                label: "Allow".to_string(),
                                description: Some("Run the tool and continue.".to_string()),
                                value: Value::String(APPROVAL_ACCEPT_ONCE_VALUE.to_string()),
                            },
                            McpServerElicitationOption {
                                label: "Deny".to_string(),
                                description: Some(
                                    "Decline this tool call and continue.".to_string(),
                                ),
                                value: Value::String(APPROVAL_DECLINE_VALUE.to_string()),
                            },
                            McpServerElicitationOption {
                                label: "Cancel".to_string(),
                                description: Some("Cancel this tool call".to_string()),
                                value: Value::String(APPROVAL_CANCEL_VALUE.to_string()),
                            },
                        ],
                        default_idx: Some(0),
                    },
                }],
                tool_suggestion: None,
            }
        );
    }

    #[test]
    fn empty_tool_approval_schema_uses_approval_actions() {
        let process_id = ProcessId::default();
        let request = McpServerElicitationFormRequest::from_event(
            process_id,
            form_request(
                "Allow this request?",
                empty_object_schema(),
                tool_approval_meta(&[], None, None),
            ),
        )
        .expect("expected approval fallback");

        assert_eq!(
            request,
            McpServerElicitationFormRequest {
                process_id,
                server_name: "server-1".to_string(),
                request_id: McpRequestId::String("request-1".to_string()),
                message: "Allow this request?".to_string(),
                approval_display_params: Vec::new(),
                response_mode: McpServerElicitationResponseMode::ApprovalAction,
                fields: vec![McpServerElicitationField {
                    id: APPROVAL_FIELD_ID.to_string(),
                    label: String::new(),
                    prompt: String::new(),
                    required: true,
                    input: McpServerElicitationFieldInput::Select {
                        options: vec![
                            McpServerElicitationOption {
                                label: "Allow".to_string(),
                                description: Some("Run the tool and continue.".to_string()),
                                value: Value::String(APPROVAL_ACCEPT_ONCE_VALUE.to_string()),
                            },
                            McpServerElicitationOption {
                                label: "Cancel".to_string(),
                                description: Some("Cancel this tool call".to_string()),
                                value: Value::String(APPROVAL_CANCEL_VALUE.to_string()),
                            },
                        ],
                        default_idx: Some(0),
                    },
                }],
                tool_suggestion: None,
            }
        );
    }

    #[test]
    fn tool_suggestion_meta_is_parsed_into_request_payload() {
        let request = McpServerElicitationFormRequest::from_event(
            ProcessId::default(),
            form_request(
                "Suggest Google Calendar",
                empty_object_schema(),
                Some(serde_json::json!({
                    "codex_approval_kind": "tool_suggestion",
                    "tool_type": "connector",
                    "suggest_type": "install",
                    "suggest_reason": "Plan and reference events from your calendar",
                    "tool_id": "connector_2128aebfecb84f64a069897515042a44",
                    "tool_name": "Google Calendar",
                    "install_url": "https://example.test/google-calendar",
                })),
            ),
        )
        .expect("expected tool suggestion form");

        assert_eq!(
            request.tool_suggestion(),
            Some(&ToolSuggestionRequest {
                tool_type: ToolSuggestionToolType::Connector,
                suggest_type: ToolSuggestionType::Install,
                suggest_reason: "Plan and reference events from your calendar".to_string(),
                tool_id: "connector_2128aebfecb84f64a069897515042a44".to_string(),
                tool_name: "Google Calendar".to_string(),
                install_url: "https://example.test/google-calendar".to_string(),
            })
        );
    }

    #[test]
    fn empty_unmarked_schema_falls_back() {
        let request = McpServerElicitationFormRequest::from_event(
            ProcessId::default(),
            form_request("Empty form", empty_object_schema(), None),
        );

        assert_eq!(request, None);
    }

    #[test]
    fn tool_approval_display_params_prefer_explicit_display_order() {
        let request = McpServerElicitationFormRequest::from_event(
            ProcessId::default(),
            form_request(
                "Allow Calendar to create an event",
                empty_object_schema(),
                tool_approval_meta(
                    &[],
                    Some(serde_json::json!({
                        "zeta": 3,
                        "alpha": 1,
                    })),
                    Some(vec![
                        (
                            "calendar_id",
                            Value::String("primary".to_string()),
                            "Calendar",
                        ),
                        (
                            "title",
                            Value::String("Roadmap review".to_string()),
                            "Title",
                        ),
                    ]),
                ),
            ),
        )
        .expect("expected approval fallback");

        assert_eq!(
            request.approval_display_params,
            vec![
                McpToolApprovalDisplayParam {
                    name: "calendar_id".to_string(),
                    value: Value::String("primary".to_string()),
                    display_name: "Calendar".to_string(),
                },
                McpToolApprovalDisplayParam {
                    name: "title".to_string(),
                    value: Value::String("Roadmap review".to_string()),
                    display_name: "Title".to_string(),
                },
            ]
        );
    }

    #[test]
    fn submit_sends_accept_with_typed_content() {
        let (tx, mut rx) = test_sender();
        let process_id = ProcessId::default();
        let request = McpServerElicitationFormRequest::from_event(
            process_id,
            form_request(
                "Allow this request?",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "confirmed": {
                            "type": "boolean",
                            "title": "Confirm",
                            "description": "Approve the pending action.",
                        }
                    },
                    "required": ["confirmed"],
                }),
                None,
            ),
        )
        .expect("expected supported form");
        let mut overlay = McpServerElicitationOverlay::new(request, tx, true, false, false);

        overlay.select_current_option(true);
        overlay.submit_answers();

        let event = rx.try_recv().expect("expected resolution");
        let AppEvent::SubmitProcessOp {
            process_id: resolved_process_id,
            op,
        } = event
        else {
            panic!("expected SubmitProcessOp");
        };
        assert_eq!(resolved_process_id, process_id);
        assert_eq!(
            op,
            Op::ResolveElicitation {
                server_name: "server-1".to_string(),
                request_id: McpRequestId::String("request-1".to_string()),
                decision: ElicitationAction::Accept,
                content: Some(serde_json::json!({
                    "confirmed": true,
                })),
                meta: None,
            }
        );
    }

    #[test]
    fn empty_tool_approval_schema_session_choice_sets_persist_meta() {
        let (tx, mut rx) = test_sender();
        let process_id = ProcessId::default();
        let request = McpServerElicitationFormRequest::from_event(
            process_id,
            form_request(
                "Allow this request?",
                empty_object_schema(),
                tool_approval_meta(
                    &[
                        APPROVAL_PERSIST_SESSION_VALUE,
                        APPROVAL_PERSIST_ALWAYS_VALUE,
                    ],
                    None,
                    None,
                ),
            ),
        )
        .expect("expected approval fallback");
        let mut overlay = McpServerElicitationOverlay::new(request, tx, true, false, false);

        if let Some(answer) = overlay.current_answer_mut() {
            answer.selection.selected_idx = Some(1);
        }
        overlay.select_current_option(true);
        overlay.submit_answers();

        let event = rx.try_recv().expect("expected resolution");
        let AppEvent::SubmitProcessOp {
            process_id: resolved_process_id,
            op,
        } = event
        else {
            panic!("expected SubmitProcessOp");
        };
        assert_eq!(resolved_process_id, process_id);
        assert_eq!(
            op,
            Op::ResolveElicitation {
                server_name: "server-1".to_string(),
                request_id: McpRequestId::String("request-1".to_string()),
                decision: ElicitationAction::Accept,
                content: None,
                meta: Some(serde_json::json!({
                    APPROVAL_PERSIST_KEY: APPROVAL_PERSIST_SESSION_VALUE,
                })),
            }
        );
    }

    #[test]
    fn empty_tool_approval_schema_always_allow_sets_persist_meta() {
        let (tx, mut rx) = test_sender();
        let process_id = ProcessId::default();
        let request = McpServerElicitationFormRequest::from_event(
            process_id,
            form_request(
                "Allow this request?",
                empty_object_schema(),
                tool_approval_meta(
                    &[
                        APPROVAL_PERSIST_SESSION_VALUE,
                        APPROVAL_PERSIST_ALWAYS_VALUE,
                    ],
                    None,
                    None,
                ),
            ),
        )
        .expect("expected approval fallback");
        let mut overlay = McpServerElicitationOverlay::new(request, tx, true, false, false);

        if let Some(answer) = overlay.current_answer_mut() {
            answer.selection.selected_idx = Some(2);
        }
        overlay.select_current_option(true);
        overlay.submit_answers();

        let event = rx.try_recv().expect("expected resolution");
        let AppEvent::SubmitProcessOp {
            process_id: resolved_process_id,
            op,
        } = event
        else {
            panic!("expected SubmitProcessOp");
        };
        assert_eq!(resolved_process_id, process_id);
        assert_eq!(
            op,
            Op::ResolveElicitation {
                server_name: "server-1".to_string(),
                request_id: McpRequestId::String("request-1".to_string()),
                decision: ElicitationAction::Accept,
                content: None,
                meta: Some(serde_json::json!({
                    APPROVAL_PERSIST_KEY: APPROVAL_PERSIST_ALWAYS_VALUE,
                })),
            }
        );
    }

    #[test]
    fn ctrl_c_cancels_elicitation() {
        use crate::bottom_pane::bottom_pane_view::BottomPaneView;
        let (tx, mut rx) = test_sender();
        let process_id = ProcessId::default();
        let request = McpServerElicitationFormRequest::from_event(
            process_id,
            form_request(
                "Allow this request?",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "confirmed": {
                            "type": "boolean",
                            "title": "Confirm",
                            "description": "Approve the pending action.",
                        }
                    },
                    "required": ["confirmed"],
                }),
                None,
            ),
        )
        .expect("expected supported form");
        let mut overlay = McpServerElicitationOverlay::new(request, tx, true, false, false);

        assert_eq!(
            overlay.on_ctrl_c(),
            crate::bottom_pane::CancellationEvent::Handled
        );

        let event = rx.try_recv().expect("expected resolution");
        let AppEvent::SubmitProcessOp {
            process_id: resolved_process_id,
            op,
        } = event
        else {
            panic!("expected SubmitProcessOp");
        };
        assert_eq!(resolved_process_id, process_id);
        assert_eq!(
            op,
            Op::ResolveElicitation {
                server_name: "server-1".to_string(),
                request_id: McpRequestId::String("request-1".to_string()),
                decision: ElicitationAction::Cancel,
                content: None,
                meta: None,
            }
        );
    }

    #[test]
    fn queues_requests_fifo() {
        use crate::bottom_pane::bottom_pane_view::BottomPaneView;
        let (tx, _rx) = test_sender();
        let first = McpServerElicitationFormRequest::from_event(
            ProcessId::default(),
            form_request(
                "First",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "confirmed": {
                            "type": "boolean",
                            "title": "Confirm",
                        }
                    },
                }),
                None,
            ),
        )
        .expect("expected supported form");
        let second = McpServerElicitationFormRequest::from_event(
            ProcessId::default(),
            form_request(
                "Second",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "confirmed": {
                            "type": "boolean",
                            "title": "Confirm",
                        }
                    },
                }),
                None,
            ),
        )
        .expect("expected supported form");
        let third = McpServerElicitationFormRequest::from_event(
            ProcessId::default(),
            form_request(
                "Third",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "confirmed": {
                            "type": "boolean",
                            "title": "Confirm",
                        }
                    },
                }),
                None,
            ),
        )
        .expect("expected supported form");
        let mut overlay = McpServerElicitationOverlay::new(first, tx, true, false, false);

        overlay.try_consume_mcp_server_elicitation_request(second);
        overlay.try_consume_mcp_server_elicitation_request(third);
        overlay.select_current_option(true);
        overlay.submit_answers();

        assert_eq!(overlay.request.message, "Second");

        overlay.select_current_option(true);
        overlay.submit_answers();

        assert_eq!(overlay.request.message, "Third");
    }

    #[test]
    fn boolean_form_snapshot() {
        let (tx, _rx) = test_sender();
        let request = McpServerElicitationFormRequest::from_event(
            ProcessId::default(),
            form_request(
                "Allow this request?",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "confirmed": {
                            "type": "boolean",
                            "title": "Confirm",
                            "description": "Approve the pending action.",
                        }
                    },
                    "required": ["confirmed"],
                }),
                None,
            ),
        )
        .expect("expected supported form");
        let overlay = McpServerElicitationOverlay::new(request, tx, true, false, false);

        insta::assert_snapshot!(
            "mcp_server_elicitation_boolean_form",
            render_to_first_char_string(&overlay, Rect::new(0, 0, 120, 16))
        );
    }

    #[test]
    fn approval_form_tool_approval_snapshot() {
        let (tx, _rx) = test_sender();
        let request = McpServerElicitationFormRequest::from_event(
            ProcessId::default(),
            form_request(
                "Allow this request?",
                empty_object_schema(),
                tool_approval_meta(&[], None, None),
            ),
        )
        .expect("expected approval fallback");
        let overlay = McpServerElicitationOverlay::new(request, tx, true, false, false);

        insta::assert_snapshot!(
            "mcp_server_elicitation_approval_form_without_schema",
            render_to_first_char_string(&overlay, Rect::new(0, 0, 120, 16))
        );
    }

    #[test]
    fn approval_form_tool_approval_with_persist_options_snapshot() {
        let (tx, _rx) = test_sender();
        let request = McpServerElicitationFormRequest::from_event(
            ProcessId::default(),
            form_request(
                "Allow this request?",
                empty_object_schema(),
                tool_approval_meta(
                    &[
                        APPROVAL_PERSIST_SESSION_VALUE,
                        APPROVAL_PERSIST_ALWAYS_VALUE,
                    ],
                    None,
                    None,
                ),
            ),
        )
        .expect("expected approval fallback");
        let overlay = McpServerElicitationOverlay::new(request, tx, true, false, false);

        insta::assert_snapshot!(
            "mcp_server_elicitation_approval_form_with_session_persist",
            render_to_first_char_string(&overlay, Rect::new(0, 0, 120, 16))
        );
    }

    #[test]
    fn approval_form_tool_approval_with_param_summary_snapshot() {
        let (tx, _rx) = test_sender();
        let request = McpServerElicitationFormRequest::from_event(
            ProcessId::default(),
            form_request(
                "Allow Calendar to create an event",
                empty_object_schema(),
                tool_approval_meta(
                    &[],
                    Some(serde_json::json!({
                        "calendar_id": "primary",
                        "title": "Roadmap review",
                        "notes": "This is a deliberately long note that should truncate before it turns the approval body into a giant wall of text in the TUI overlay.",
                        "ignored_after_limit": "fourth param",
                    })),
                    Some(vec![
                        (
                            "calendar_id",
                            Value::String("primary".to_string()),
                            "Calendar",
                        ),
                        (
                            "title",
                            Value::String("Roadmap review".to_string()),
                            "Title",
                        ),
                        (
                            "notes",
                            Value::String("This is a deliberately long note that should truncate before it turns the approval body into a giant wall of text in the TUI overlay.".to_string()),
                            "Notes",
                        ),
                        (
                            "ignored_after_limit",
                            Value::String("fourth param".to_string()),
                            "Ignored",
                        ),
                    ]),
                ),
            ),
        )
        .expect("expected approval fallback");
        let overlay = McpServerElicitationOverlay::new(request, tx, true, false, false);

        insta::assert_snapshot!(
            "mcp_server_elicitation_approval_form_with_param_summary",
            render_to_first_char_string(&overlay, Rect::new(0, 0, 120, 16))
        );
    }
}
