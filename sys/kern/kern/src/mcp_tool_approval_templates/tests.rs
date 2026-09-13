use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;

#[test]
fn renders_exact_match_with_readable_param_labels() {
    let templates = vec![ConsequentialToolMessageTemplate {
        connector_id: "calendar".to_string(),
        server_name: "codex_apps".to_string(),
        tool_title: "create_event".to_string(),
        template: "Allow {connector_name} to create an event?".to_string(),
        template_params: vec![
            ConsequentialToolTemplateParam {
                name: "calendar_id".to_string(),
                label: "Calendar".to_string(),
            },
            ConsequentialToolTemplateParam {
                name: "title".to_string(),
                label: "Title".to_string(),
            },
        ],
    }];

    let rendered = render_mcp_tool_approval_template_from_templates(
        &templates,
        "codex_apps",
        Some("calendar"),
        Some("Calendar"),
        Some("create_event"),
        Some(&json!({
            "title": "Roadmap review",
            "calendar_id": "primary",
            "timezone": "UTC",
        })),
    );

    assert_eq!(
        rendered,
        Some(RenderedMcpToolApprovalTemplate {
            question: "Allow Calendar to create an event?".to_string(),
            elicitation_message: "Allow Calendar to create an event?".to_string(),
            tool_params: Some(json!({
                "title": "Roadmap review",
                "calendar_id": "primary",
                "timezone": "UTC",
            })),
            tool_params_display: vec![
                RenderedMcpToolApprovalParam {
                    name: "calendar_id".to_string(),
                    value: json!("primary"),
                    display_name: "Calendar".to_string(),
                },
                RenderedMcpToolApprovalParam {
                    name: "title".to_string(),
                    value: json!("Roadmap review"),
                    display_name: "Title".to_string(),
                },
                RenderedMcpToolApprovalParam {
                    name: "timezone".to_string(),
                    value: json!("UTC"),
                    display_name: "timezone".to_string(),
                },
            ],
        })
    );
}

#[test]
fn returns_none_when_no_exact_match_exists() {
    let templates = vec![ConsequentialToolMessageTemplate {
        connector_id: "calendar".to_string(),
        server_name: "codex_apps".to_string(),
        tool_title: "create_event".to_string(),
        template: "Allow {connector_name} to create an event?".to_string(),
        template_params: Vec::new(),
    }];

    assert_eq!(
        render_mcp_tool_approval_template_from_templates(
            &templates,
            "codex_apps",
            Some("calendar"),
            Some("Calendar"),
            Some("delete_event"),
            Some(&json!({})),
        ),
        None
    );
}

#[test]
fn returns_none_when_relabeling_would_collide() {
    let templates = vec![ConsequentialToolMessageTemplate {
        connector_id: "calendar".to_string(),
        server_name: "codex_apps".to_string(),
        tool_title: "create_event".to_string(),
        template: "Allow {connector_name} to create an event?".to_string(),
        template_params: vec![ConsequentialToolTemplateParam {
            name: "calendar_id".to_string(),
            label: "timezone".to_string(),
        }],
    }];

    assert_eq!(
        render_mcp_tool_approval_template_from_templates(
            &templates,
            "codex_apps",
            Some("calendar"),
            Some("Calendar"),
            Some("create_event"),
            Some(&json!({
                "calendar_id": "primary",
                "timezone": "UTC",
            })),
        ),
        None
    );
}

#[test]
fn bundled_templates_load() {
    assert_eq!(CONSEQUENTIAL_TOOL_MESSAGE_TEMPLATES.is_some(), true);
}

#[test]
fn renders_literal_template_without_connector_substitution() {
    let templates = vec![ConsequentialToolMessageTemplate {
        connector_id: "github".to_string(),
        server_name: "codex_apps".to_string(),
        tool_title: "add_comment".to_string(),
        template: "Allow GitHub to add a comment to a pull request?".to_string(),
        template_params: Vec::new(),
    }];

    let rendered = render_mcp_tool_approval_template_from_templates(
        &templates,
        "codex_apps",
        Some("github"),
        None,
        Some("add_comment"),
        Some(&json!({})),
    );

    assert_eq!(
        rendered,
        Some(RenderedMcpToolApprovalTemplate {
            question: "Allow GitHub to add a comment to a pull request?".to_string(),
            elicitation_message: "Allow GitHub to add a comment to a pull request?".to_string(),
            tool_params: Some(json!({})),
            tool_params_display: Vec::new(),
        })
    );
}

#[test]
fn returns_none_when_connector_placeholder_has_no_value() {
    let templates = vec![ConsequentialToolMessageTemplate {
        connector_id: "calendar".to_string(),
        server_name: "codex_apps".to_string(),
        tool_title: "create_event".to_string(),
        template: "Allow {connector_name} to create an event?".to_string(),
        template_params: Vec::new(),
    }];

    assert_eq!(
        render_mcp_tool_approval_template_from_templates(
            &templates,
            "codex_apps",
            Some("calendar"),
            None,
            Some("create_event"),
            Some(&json!({})),
        ),
        None
    );
}
