use super::*;
use crate::chaos::make_session_and_context;

/// Test-local stand-in: the real constant was removed because all MCP servers
/// are now treated equally. Tests that were written against the old apps server
/// keep this name so the approval/metadata plumbing is still exercised.
const CHAOS_APPS_MCP_SERVER_NAME: &str = "test-apps-server";
use crate::config::types::AppConfig;
use crate::config::types::AppToolConfig;
use crate::config::types::AppToolsConfig;
use crate::config::types::McpToolApprovalServerConfig;
use chaos_ipc::api::ConfigLayerSource;
use chaos_realpath::AbsolutePathBuf;
use chaos_sysctl::CONFIG_TOML_FILE;
use chaos_sysctl::ConfigLayerEntry;
use chaos_sysctl::ConfigLayerStack;
use chaos_sysctl::ConfigRequirements;
use chaos_sysctl::ConfigRequirementsToml;
use pretty_assertions::assert_eq;
use serial_test::serial;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::tempdir;

fn annotations(
    read_only: Option<bool>,
    destructive: Option<bool>,
    open_world: Option<bool>,
) -> ToolAnnotations {
    ToolAnnotations {
        destructive_hint: destructive,
        idempotent_hint: None,
        open_world_hint: open_world,
        read_only_hint: read_only,
        title: None,
    }
}

fn approval_metadata(
    connector_id: Option<&str>,
    connector_name: Option<&str>,
    connector_description: Option<&str>,
    tool_title: Option<&str>,
    tool_description: Option<&str>,
) -> McpToolApprovalMetadata {
    McpToolApprovalMetadata {
        annotations: None,
        connector_id: connector_id.map(str::to_string),
        connector_name: connector_name.map(str::to_string),
        connector_description: connector_description.map(str::to_string),
        tool_title: tool_title.map(str::to_string),
        tool_description: tool_description.map(str::to_string),
        codex_apps_meta: None,
    }
}

fn prompt_options(
    allow_session_remember: bool,
    allow_persistent_approval: bool,
) -> McpToolApprovalPromptOptions {
    McpToolApprovalPromptOptions {
        allow_session_remember,
        allow_persistent_approval,
    }
}

#[test]
fn approval_required_when_read_only_false_and_destructive() {
    let annotations = annotations(Some(false), Some(true), None);
    assert_eq!(requires_mcp_tool_approval(Some(&annotations)), true);
}

#[test]
fn approval_required_when_read_only_false_and_open_world() {
    let annotations = annotations(Some(false), None, Some(true));
    assert_eq!(requires_mcp_tool_approval(Some(&annotations)), true);
}

#[test]
fn approval_required_when_destructive_even_if_read_only_true() {
    let annotations = annotations(Some(true), Some(true), Some(true));
    assert_eq!(requires_mcp_tool_approval(Some(&annotations)), true);
}

#[test]
fn approval_required_when_annotations_missing() {
    assert_eq!(requires_mcp_tool_approval(None), true);
}

#[test]
fn approval_not_required_when_explicitly_read_only() {
    let annotations = annotations(Some(true), None, None);
    assert_eq!(requires_mcp_tool_approval(Some(&annotations)), false);
}

#[test]
fn prompt_mode_does_not_allow_persistent_remember() {
    assert_eq!(
        normalize_approval_decision_for_mode(
            McpToolApprovalDecision::AcceptForSession,
            AppToolApproval::Prompt,
        ),
        McpToolApprovalDecision::Accept
    );
    assert_eq!(
        normalize_approval_decision_for_mode(
            McpToolApprovalDecision::AcceptAndRemember,
            AppToolApproval::Prompt,
        ),
        McpToolApprovalDecision::Accept
    );
}

#[test]
fn approval_question_text_prepends_safety_reason() {
    assert_eq!(
        mcp_tool_approval_question_text(
            "Allow this action?".to_string(),
            Some("This tool may contact an external system."),
        ),
        "Tool call needs your approval. Reason: This tool may contact an external system."
    );
}

#[tokio::test]
async fn approval_elicitation_request_uses_message_override_and_preserves_tool_params_keys() {
    let (session, turn_context) = make_session_and_context().await;
    let question = build_mcp_tool_approval_question(
        "q".to_string(),
        CHAOS_APPS_MCP_SERVER_NAME,
        "create_event",
        Some("Calendar"),
        prompt_options(true, true),
        Some("Allow Calendar to create an event?"),
    );

    let request = build_mcp_tool_approval_elicitation_request(
        &session,
        &turn_context,
        McpToolApprovalElicitationRequest {
            server: CHAOS_APPS_MCP_SERVER_NAME,
            metadata: Some(&approval_metadata(
                Some("calendar"),
                Some("Calendar"),
                Some("Manage events and schedules."),
                Some("Create Event"),
                Some("Create a calendar event."),
            )),
            tool_params: Some(&serde_json::json!({
                "calendar_id": "primary",
                "title": "Roadmap review",
            })),
            tool_params_display: Some(&[
                RenderedMcpToolApprovalParam {
                    name: "calendar_id".to_string(),
                    value: serde_json::json!("primary"),
                    display_name: "Calendar".to_string(),
                },
                RenderedMcpToolApprovalParam {
                    name: "title".to_string(),
                    value: serde_json::json!("Roadmap review"),
                    display_name: "Title".to_string(),
                },
            ]),
            question,
            message_override: Some("Allow Calendar to create an event?"),
            prompt_options: prompt_options(true, true),
        },
    );

    assert_eq!(
        request,
        McpServerElicitationRequestParams {
            process_id: session.conversation_id.to_string(),
            turn_id: Some(turn_context.sub_id),
            server_name: CHAOS_APPS_MCP_SERVER_NAME.to_string(),
            request: McpServerElicitationRequest::Form {
                meta: Some(serde_json::json!({
                    MCP_TOOL_APPROVAL_KIND_KEY: MCP_TOOL_APPROVAL_KIND_MCP_TOOL_CALL,
                    MCP_TOOL_APPROVAL_PERSIST_KEY: [
                        MCP_TOOL_APPROVAL_PERSIST_SESSION,
                        MCP_TOOL_APPROVAL_PERSIST_ALWAYS,
                    ],
                    MCP_TOOL_APPROVAL_SOURCE_KEY: MCP_TOOL_APPROVAL_SOURCE_CONNECTOR,
                    MCP_TOOL_APPROVAL_CONNECTOR_ID_KEY: "calendar",
                    MCP_TOOL_APPROVAL_CONNECTOR_NAME_KEY: "Calendar",
                    MCP_TOOL_APPROVAL_CONNECTOR_DESCRIPTION_KEY: "Manage events and schedules.",
                    MCP_TOOL_APPROVAL_TOOL_TITLE_KEY: "Create Event",
                    MCP_TOOL_APPROVAL_TOOL_DESCRIPTION_KEY: "Create a calendar event.",
                    MCP_TOOL_APPROVAL_TOOL_PARAMS_KEY: {
                        "calendar_id": "primary",
                        "title": "Roadmap review",
                    },
                    MCP_TOOL_APPROVAL_TOOL_PARAMS_DISPLAY_KEY: [
                        {
                            "name": "calendar_id",
                            "value": "primary",
                            "display_name": "Calendar",
                        },
                        {
                            "name": "title",
                            "value": "Roadmap review",
                            "display_name": "Title",
                        },
                    ],
                })),
                message: "Allow Calendar to create an event?".to_string(),
                requested_schema: McpElicitationSchema {
                    schema_uri: None,
                    type_: McpElicitationObjectType::Object,
                    properties: BTreeMap::new(),
                    required: None,
                },
            },
        }
    );
}

#[test]
fn custom_mcp_tool_question_mentions_server_and_offers_persistence() {
    let question = build_mcp_tool_approval_question(
        "q".to_string(),
        "custom_server",
        "run_action",
        None,
        prompt_options(true, true),
        None,
    );

    assert_eq!(question.header, "Approve app tool call?");
    assert_eq!(
        question.question,
        "Allow the custom_server MCP server to run tool \"run_action\"?"
    );
    assert!(
        question
            .options
            .expect("options")
            .into_iter()
            .map(|option| option.label)
            .any(|label| label == MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER)
    );
}

#[test]
fn codex_apps_tool_question_uses_fallback_app_label() {
    let question = build_mcp_tool_approval_question(
        "q".to_string(),
        CHAOS_APPS_MCP_SERVER_NAME,
        "run_action",
        None,
        prompt_options(true, true),
        None,
    );

    assert_eq!(
        question.question,
        "Allow the test-apps-server MCP server to run tool \"run_action\"?"
    );
}

#[test]
fn trusted_codex_apps_tool_question_offers_always_allow() {
    let question = build_mcp_tool_approval_question(
        "q".to_string(),
        CHAOS_APPS_MCP_SERVER_NAME,
        "run_action",
        Some("Calendar"),
        prompt_options(true, true),
        None,
    );
    let options = question.options.expect("options");

    assert!(options.iter().any(|option| {
        option.label == MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION
            && option.description == "Run the tool and remember this choice for this session."
    }));
    assert!(options.iter().any(|option| {
        option.label == MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER
            && option.description == "Run the tool and remember this choice for future tool calls."
    }));
    assert_eq!(
        options
            .into_iter()
            .map(|option| option.label)
            .collect::<Vec<_>>(),
        vec![
            MCP_TOOL_APPROVAL_ACCEPT.to_string(),
            MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION.to_string(),
            MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER.to_string(),
            MCP_TOOL_APPROVAL_CANCEL.to_string(),
        ]
    );
}

#[test]
fn custom_mcp_tool_question_offers_session_remember_without_always_allow() {
    let question = build_mcp_tool_approval_question(
        "q".to_string(),
        "custom_server",
        "run_action",
        None,
        prompt_options(true, false),
        None,
    );

    assert_eq!(
        question
            .options
            .expect("options")
            .into_iter()
            .map(|option| option.label)
            .collect::<Vec<_>>(),
        vec![
            MCP_TOOL_APPROVAL_ACCEPT.to_string(),
            MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION.to_string(),
            MCP_TOOL_APPROVAL_CANCEL.to_string(),
        ]
    );
}

#[test]
fn custom_servers_support_persistent_approval() {
    let invocation = McpInvocation {
        server: Some("custom_server".to_string()),
        tool: "run_action".to_string(),
        arguments: None,
    };
    let expected = McpToolApprovalKey {
        server: "custom_server".to_string(),
        connector_id: None,
        tool_name: "run_action".to_string(),
    };

    assert_eq!(
        session_mcp_tool_approval_key(&invocation, None, AppToolApproval::Auto),
        Some(expected.clone())
    );
    assert_eq!(
        persistent_mcp_tool_approval_key(&invocation, None, AppToolApproval::Auto),
        Some(expected)
    );
}

#[test]
fn personal_and_connector_approval_modes_resolve_from_most_specific_to_default() {
    let personal = McpToolApprovalServerConfig {
        approval_mode: Some(AppToolApproval::Approve),
        tools: HashMap::from([("evaluate".to_string(), AppToolApproval::Prompt)]),
    };
    let connector = AppConfig {
        enabled: true,
        destructive_enabled: None,
        open_world_enabled: None,
        default_tools_approval_mode: Some(AppToolApproval::Prompt),
        default_tools_enabled: None,
        tools: Some(AppToolsConfig {
            tools: HashMap::from([(
                "evaluate".to_string(),
                AppToolConfig {
                    enabled: None,
                    approval_mode: Some(AppToolApproval::Auto),
                },
            )]),
        }),
    };

    assert_eq!(
        resolve_mcp_tool_approval_mode(None, Some(&personal), "evaluate"),
        AppToolApproval::Prompt
    );
    assert_eq!(
        resolve_mcp_tool_approval_mode(None, Some(&personal), "navigate"),
        AppToolApproval::Approve
    );
    assert_eq!(
        resolve_mcp_tool_approval_mode(Some(&connector), Some(&personal), "evaluate"),
        AppToolApproval::Auto
    );
    assert_eq!(
        resolve_mcp_tool_approval_mode(Some(&connector), Some(&personal), "navigate"),
        AppToolApproval::Prompt
    );
}

#[test]
fn configured_approvals_are_read_from_user_config() {
    let user_config: toml::Value = toml::from_str(
        r#"
        [apps.calendar]
        default_tools_approval_mode = "prompt"

        [apps.calendar.tools.create_event]
        approval_mode = "approve"

        [mcp_tool_approvals.chrome]
        approval_mode = "approve"

        [mcp_tool_approvals.chrome.tools]
        evaluate = "prompt"
        "#,
    )
    .expect("valid effective config");

    let connector = configured_connector(&user_config, "calendar").expect("calendar connector");
    let personal =
        configured_personal_mcp_approval(&user_config, "chrome").expect("chrome approvals");

    assert_eq!(
        resolve_mcp_tool_approval_mode(Some(&connector), None, "create_event"),
        AppToolApproval::Approve
    );
    assert_eq!(
        resolve_mcp_tool_approval_mode(Some(&connector), None, "list_events"),
        AppToolApproval::Prompt
    );
    assert_eq!(
        resolve_mcp_tool_approval_mode(None, Some(&personal), "evaluate"),
        AppToolApproval::Prompt
    );
    assert_eq!(
        resolve_mcp_tool_approval_mode(None, Some(&personal), "navigate"),
        AppToolApproval::Approve
    );
}

#[tokio::test]
async fn personal_mcp_approvals_only_use_the_user_config_layer() {
    let tmp = tempdir().expect("tempdir");
    let user_config_path =
        AbsolutePathBuf::try_from(tmp.path().join("config.toml")).expect("absolute user config");
    let project_config_dir =
        AbsolutePathBuf::try_from(tmp.path().join("project/.chaos")).expect("absolute project dir");
    let user_config: toml::Value = toml::from_str(
        r#"
        [apps.calendar]
        default_tools_approval_mode = "prompt"

        [mcp_tool_approvals.chrome.tools]
        navigate = "prompt"
        "#,
    )
    .expect("valid user config");
    let project_config: toml::Value = toml::from_str(
        r#"
        [apps.calendar]
        default_tools_approval_mode = "approve"

        [mcp_tool_approvals.chrome.tools]
        navigate = "approve"
        "#,
    )
    .expect("valid project config");
    let stack = ConfigLayerStack::new(
        vec![
            ConfigLayerEntry::new(
                ConfigLayerSource::User {
                    file: user_config_path,
                },
                user_config,
            ),
            ConfigLayerEntry::new(
                ConfigLayerSource::Project {
                    dot_codex_folder: project_config_dir,
                },
                project_config,
            ),
        ],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("valid layer stack");

    let (_, mut turn_context) = make_session_and_context().await;
    let mut config = (*turn_context.config).clone();
    config.config_layer_stack = stack;
    turn_context.config = Arc::new(config);
    let invocation = McpInvocation {
        server: Some("chrome".to_string()),
        tool: "navigate".to_string(),
        arguments: None,
    };

    assert_eq!(
        configured_mcp_tool_approval_mode(&turn_context, &invocation, None),
        AppToolApproval::Prompt
    );

    let connector_metadata =
        approval_metadata(Some("calendar"), Some("Calendar"), None, None, None);
    assert_eq!(
        configured_mcp_tool_approval_mode(
            &turn_context,
            &McpInvocation {
                server: Some(CHAOS_APPS_MCP_SERVER_NAME.to_string()),
                tool: "create_event".to_string(),
                arguments: None,
            },
            Some(&connector_metadata),
        ),
        AppToolApproval::Prompt
    );
}

#[test]
fn codex_apps_connectors_support_persistent_approval() {
    let invocation = McpInvocation {
        server: Some(CHAOS_APPS_MCP_SERVER_NAME.to_string()),
        tool: "calendar/list_events".to_string(),
        arguments: None,
    };
    let metadata = approval_metadata(Some("calendar"), Some("Calendar"), None, None, None);
    let expected = McpToolApprovalKey {
        server: CHAOS_APPS_MCP_SERVER_NAME.to_string(),
        connector_id: Some("calendar".to_string()),
        tool_name: "calendar/list_events".to_string(),
    };

    assert_eq!(
        session_mcp_tool_approval_key(&invocation, Some(&metadata), AppToolApproval::Auto),
        Some(expected.clone())
    );
    assert_eq!(
        persistent_mcp_tool_approval_key(&invocation, Some(&metadata), AppToolApproval::Auto),
        Some(expected)
    );
}

#[test]
fn sanitize_mcp_tool_result_for_model_rewrites_image_content() {
    let result = Ok(CallToolResult {
        content: vec![
            serde_json::json!({
                "type": "image",
                "data": "Zm9v",
                "mimeType": "image/png",
            }),
            serde_json::json!({
                "type": "text",
                "text": "hello",
            }),
        ],
        structured_content: None,
        is_error: Some(false),
        meta: None,
    });

    let got = sanitize_mcp_tool_result_for_model(false, result).expect("sanitized result");

    assert_eq!(
        got.content,
        vec![
            serde_json::json!({
                "type": "text",
                "text": "<image content omitted because you do not support image input>",
            }),
            serde_json::json!({
                "type": "text",
                "text": "hello",
            }),
        ]
    );
}

#[test]
fn sanitize_mcp_tool_result_for_model_uses_structured_content_only_when_present() {
    let original = CallToolResult {
        content: vec![serde_json::json!({
            "type": "image",
            "data": "Zm9v",
            "mimeType": "image/png",
        })],
        structured_content: Some(serde_json::json!({"x": 1})),
        is_error: Some(false),
        meta: Some(serde_json::json!({"k": "v"})),
    };

    let got =
        sanitize_mcp_tool_result_for_model(true, Ok(original.clone())).expect("unsanitized result");

    assert!(got.content.is_empty());
    assert_eq!(got.structured_content, original.structured_content);
    assert_eq!(got.is_error, original.is_error);
    assert_eq!(got.meta, original.meta);
}

#[test]
fn codex_apps_tool_call_request_meta_includes_codex_apps_meta() {
    let metadata = McpToolApprovalMetadata {
        annotations: None,
        connector_id: Some("calendar".to_string()),
        connector_name: Some("Calendar".to_string()),
        connector_description: Some("Manage events".to_string()),
        tool_title: Some("Create Event".to_string()),
        tool_description: Some("Create a calendar event.".to_string()),
        codex_apps_meta: Some(
            serde_json::json!({
                "resource_uri": "connector://calendar/tools/calendar_create_event",
                "contains_mcp_source": true,
                "connector_id": "calendar",
            })
            .as_object()
            .cloned()
            .expect("_codex_apps metadata should be an object"),
        ),
    };

    assert_eq!(
        build_mcp_tool_call_request_meta(CHAOS_APPS_MCP_SERVER_NAME, Some(&metadata)),
        Some(serde_json::json!({
            MCP_TOOL_CHAOS_APPS_META_KEY: {
                "resource_uri": "connector://calendar/tools/calendar_create_event",
                "contains_mcp_source": true,
                "connector_id": "calendar",
            },
        }))
    );
}

#[test]
fn accepted_elicitation_content_converts_to_request_user_input_response() {
    let response = request_user_input_response_from_elicitation_content(Some(serde_json::json!(
        {
            "approval": MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER,
        }
    )));

    assert_eq!(
        response,
        Some(RequestUserInputResponse {
            answers: std::collections::HashMap::from([(
                "approval".to_string(),
                RequestUserInputAnswer {
                    answers: vec![MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER.to_string()],
                },
            )]),
        })
    );
}

#[test]
fn approval_elicitation_meta_marks_tool_approvals() {
    assert_eq!(
        build_mcp_tool_approval_elicitation_meta(
            "custom_server",
            None,
            None,
            None,
            prompt_options(false, false),
        ),
        Some(serde_json::json!({
            MCP_TOOL_APPROVAL_KIND_KEY: MCP_TOOL_APPROVAL_KIND_MCP_TOOL_CALL,
        }))
    );
}

#[test]
fn approval_elicitation_meta_keeps_session_persist_behavior_for_custom_servers() {
    assert_eq!(
        build_mcp_tool_approval_elicitation_meta(
            "custom_server",
            Some(&approval_metadata(
                None,
                None,
                None,
                Some("Run Action"),
                Some("Runs the selected action."),
            )),
            Some(&serde_json::json!({"id": 1})),
            None,
            prompt_options(true, false),
        ),
        Some(serde_json::json!({
            MCP_TOOL_APPROVAL_KIND_KEY: MCP_TOOL_APPROVAL_KIND_MCP_TOOL_CALL,
            MCP_TOOL_APPROVAL_PERSIST_KEY: MCP_TOOL_APPROVAL_PERSIST_SESSION,
            MCP_TOOL_APPROVAL_TOOL_TITLE_KEY: "Run Action",
            MCP_TOOL_APPROVAL_TOOL_DESCRIPTION_KEY: "Runs the selected action.",
            MCP_TOOL_APPROVAL_TOOL_PARAMS_KEY: {
                "id": 1,
            },
        }))
    );
}

#[test]
fn approval_elicitation_meta_includes_connector_source_for_codex_apps() {
    assert_eq!(
        build_mcp_tool_approval_elicitation_meta(
            CHAOS_APPS_MCP_SERVER_NAME,
            Some(&approval_metadata(
                Some("calendar"),
                Some("Calendar"),
                Some("Manage events and schedules."),
                Some("Run Action"),
                Some("Runs the selected action."),
            )),
            Some(&serde_json::json!({
                "calendar_id": "primary",
            })),
            None,
            prompt_options(false, false),
        ),
        Some(serde_json::json!({
            MCP_TOOL_APPROVAL_KIND_KEY: MCP_TOOL_APPROVAL_KIND_MCP_TOOL_CALL,
            MCP_TOOL_APPROVAL_SOURCE_KEY: MCP_TOOL_APPROVAL_SOURCE_CONNECTOR,
            MCP_TOOL_APPROVAL_CONNECTOR_ID_KEY: "calendar",
            MCP_TOOL_APPROVAL_CONNECTOR_NAME_KEY: "Calendar",
            MCP_TOOL_APPROVAL_CONNECTOR_DESCRIPTION_KEY: "Manage events and schedules.",
            MCP_TOOL_APPROVAL_TOOL_TITLE_KEY: "Run Action",
            MCP_TOOL_APPROVAL_TOOL_DESCRIPTION_KEY: "Runs the selected action.",
            MCP_TOOL_APPROVAL_TOOL_PARAMS_KEY: {
                "calendar_id": "primary",
            },
        }))
    );
}

#[test]
fn approval_elicitation_meta_merges_session_and_always_persist_with_connector_source() {
    assert_eq!(
        build_mcp_tool_approval_elicitation_meta(
            CHAOS_APPS_MCP_SERVER_NAME,
            Some(&approval_metadata(
                Some("calendar"),
                Some("Calendar"),
                Some("Manage events and schedules."),
                Some("Run Action"),
                Some("Runs the selected action."),
            )),
            Some(&serde_json::json!({
                "calendar_id": "primary",
            })),
            None,
            prompt_options(true, true),
        ),
        Some(serde_json::json!({
            MCP_TOOL_APPROVAL_KIND_KEY: MCP_TOOL_APPROVAL_KIND_MCP_TOOL_CALL,
            MCP_TOOL_APPROVAL_PERSIST_KEY: [
                MCP_TOOL_APPROVAL_PERSIST_SESSION,
                MCP_TOOL_APPROVAL_PERSIST_ALWAYS,
            ],
            MCP_TOOL_APPROVAL_SOURCE_KEY: MCP_TOOL_APPROVAL_SOURCE_CONNECTOR,
            MCP_TOOL_APPROVAL_CONNECTOR_ID_KEY: "calendar",
            MCP_TOOL_APPROVAL_CONNECTOR_NAME_KEY: "Calendar",
            MCP_TOOL_APPROVAL_CONNECTOR_DESCRIPTION_KEY: "Manage events and schedules.",
            MCP_TOOL_APPROVAL_TOOL_TITLE_KEY: "Run Action",
            MCP_TOOL_APPROVAL_TOOL_DESCRIPTION_KEY: "Runs the selected action.",
            MCP_TOOL_APPROVAL_TOOL_PARAMS_KEY: {
                "calendar_id": "primary",
            },
        }))
    );
}

#[test]
fn declined_elicitation_response_stays_decline() {
    let response = parse_mcp_tool_approval_elicitation_response(
        Some(ElicitationResponse {
            action: ElicitationAction::Decline,
            content: Some(serde_json::json!({
                "approval": MCP_TOOL_APPROVAL_ACCEPT,
            })),
            meta: None,
        }),
        "approval",
    );

    assert_eq!(response, McpToolApprovalDecision::Decline);
}

#[test]
fn synthetic_decline_request_user_input_response_stays_decline() {
    let response = parse_mcp_tool_approval_response(
        Some(RequestUserInputResponse {
            answers: HashMap::from([(
                "approval".to_string(),
                RequestUserInputAnswer {
                    answers: vec!["Decline".to_string()],
                },
            )]),
        }),
        "approval",
    );

    assert_eq!(response, McpToolApprovalDecision::Cancel);
}

#[test]
fn accepted_elicitation_response_uses_always_persist_meta() {
    let response = parse_mcp_tool_approval_elicitation_response(
        Some(ElicitationResponse {
            action: ElicitationAction::Accept,
            content: None,
            meta: Some(serde_json::json!({
                MCP_TOOL_APPROVAL_PERSIST_KEY: MCP_TOOL_APPROVAL_PERSIST_ALWAYS,
            })),
        }),
        "approval",
    );

    assert_eq!(response, McpToolApprovalDecision::AcceptAndRemember);
}

#[test]
fn accepted_elicitation_response_uses_session_persist_meta() {
    let response = parse_mcp_tool_approval_elicitation_response(
        Some(ElicitationResponse {
            action: ElicitationAction::Accept,
            content: None,
            meta: Some(serde_json::json!({
                MCP_TOOL_APPROVAL_PERSIST_KEY: MCP_TOOL_APPROVAL_PERSIST_SESSION,
            })),
        }),
        "approval",
    );

    assert_eq!(response, McpToolApprovalDecision::AcceptForSession);
}

#[test]
fn accepted_elicitation_without_content_defaults_to_accept() {
    let response = parse_mcp_tool_approval_elicitation_response(
        Some(ElicitationResponse {
            action: ElicitationAction::Accept,
            content: None,
            meta: None,
        }),
        "approval",
    );

    assert_eq!(response, McpToolApprovalDecision::Accept);
}

#[tokio::test]
async fn persist_codex_app_tool_approval_uses_database() {
    let (_session, context) = make_session_and_context().await;
    let bound = test_bound_approval(&context, Some("calendar"));
    persist_bound_mcp_approval(&context, &bound)
        .await
        .expect("persist");
    let runtime = crate::user_settings::open(&context.config.chaos_home)
        .await
        .expect("open");
    let grants = runtime
        .list_approvals(&bound.installation_id)
        .await
        .expect("list");
    assert_eq!(grants.len(), 1);
    assert_eq!(
        grants[0].subject,
        serde_json::to_string(&bound.key).expect("key")
    );
    assert_eq!(grants[0].identity, bound.identity);
}

#[tokio::test]
async fn persist_mcp_server_tool_approval_does_not_write_toml() {
    let (_session, context) = make_session_and_context().await;
    let path = context.config.chaos_home.join(CONFIG_TOML_FILE);
    let before = std::fs::read(&path).ok();
    persist_bound_mcp_approval(&context, &test_bound_approval(&context, None))
        .await
        .expect("persist");
    assert_eq!(before, std::fs::read(&path).ok());
}

fn test_bound_approval(context: &TurnContext, connector: Option<&str>) -> BoundMcpApproval {
    BoundMcpApproval {
        key: McpToolApprovalKey {
            server: "test".into(),
            connector_id: connector.map(str::to_owned),
            tool_name: "test-tool".into(),
        },
        identity: "v1:test".into(),
        scope: "installation".into(),
        revision: 0,
        installation_id: crate::user_settings::installation_id(&context.config.chaos_home)
            .expect("installation"),
    }
}

#[test]
fn session_approval_identity_includes_revision_and_scope() {
    let mut store = crate::tools::sandboxing::ApprovalStore::default();
    let mut bound = BoundMcpApproval {
        key: McpToolApprovalKey {
            server: "test".into(),
            connector_id: None,
            tool_name: "tool".into(),
        },
        identity: "v1:a".into(),
        scope: "/a".into(),
        revision: 1,
        installation_id: "local".into(),
    };
    store.put(bound.clone(), ReviewDecision::ApprovedForSession);
    assert!(store.get(&bound).is_some());
    bound.revision += 1;
    assert!(store.get(&bound).is_none());
    bound.revision -= 1;
    bound.scope = "/b".into();
    assert!(store.get(&bound).is_none());
}

#[tokio::test]
async fn approval_without_external_server_is_cancelled() {
    let (session, turn_context) = make_session_and_context().await;
    let invocation = McpInvocation {
        server: None,
        tool: "tool".to_string(),
        arguments: None,
    };
    let decision = maybe_request_mcp_tool_approval(
        &Arc::new(session),
        &Arc::new(turn_context),
        "missing-server",
        &invocation,
        None,
        AppToolApproval::Prompt,
    )
    .await;
    assert_eq!(decision, Some(McpToolApprovalDecision::Cancel));
}

#[tokio::test]
async fn approve_mode_skips_when_annotations_do_not_require_approval() {
    let (session, turn_context) = make_session_and_context().await;
    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let invocation = McpInvocation {
        server: Some("custom_server".to_string()),
        tool: "read_only_tool".to_string(),
        arguments: None,
    };
    let metadata = McpToolApprovalMetadata {
        annotations: Some(annotations(Some(true), None, None)),
        connector_id: None,
        connector_name: None,
        connector_description: None,
        tool_title: Some("Read Only Tool".to_string()),
        tool_description: None,
        codex_apps_meta: None,
    };

    let decision = maybe_request_mcp_tool_approval(
        &session,
        &turn_context,
        "call-1",
        &invocation,
        Some(&metadata),
        AppToolApproval::Approve,
    )
    .await;

    assert_eq!(decision, None);
}

#[tokio::test]
#[serial(arc_monitor_server)]
async fn approve_mode_blocks_when_arc_returns_interrupt_for_model() {
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chaos/safety/arc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "outcome": "steer-model",
            "short_reason": "needs approval",
            "rationale": "high-risk action",
            "risk_score": 96,
            "risk_level": "critical",
            "evidence": [{
                "message": "dangerous_tool",
                "why": "high-risk action",
            }],
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (session, mut turn_context) = make_session_and_context().await;
    turn_context.auth_manager = Some(crate::test_support::auth_manager_from_auth(
        crate::ChaosAuth::create_dummy_chatgpt_auth_for_testing(),
    ));
    let mut config = (*turn_context.config).clone();
    config.chatgpt_base_url = server.uri();
    turn_context.config = Arc::new(config);

    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let invocation = McpInvocation {
        server: Some(CHAOS_APPS_MCP_SERVER_NAME.to_string()),
        tool: "dangerous_tool".to_string(),
        arguments: Some(serde_json::json!({ "id": 1 })),
    };
    let metadata = McpToolApprovalMetadata {
        annotations: Some(annotations(Some(false), Some(true), Some(true))),
        connector_id: Some("calendar".to_string()),
        connector_name: Some("Calendar".to_string()),
        connector_description: Some("Manage events".to_string()),
        tool_title: Some("Dangerous Tool".to_string()),
        tool_description: Some("Performs a risky action.".to_string()),
        codex_apps_meta: None,
    };

    let decision = maybe_request_mcp_tool_approval(
        &session,
        &turn_context,
        "call-2",
        &invocation,
        Some(&metadata),
        AppToolApproval::Approve,
    )
    .await;

    assert_eq!(
        decision,
        Some(McpToolApprovalDecision::BlockedBySafetyMonitor(
            "Tool call was cancelled because of safety risks: high-risk action".to_string(),
        ))
    );
}
