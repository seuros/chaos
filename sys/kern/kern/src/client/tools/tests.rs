use super::*;
use chaos_ipc::protocol::GranularApprovalConfig;
use std::path::Path;

#[test]
fn headless_bypasses_permission_prompt() {
    assert!(clamp_bypasses_permission_prompt(ApprovalPolicy::Headless));
}

#[test]
fn interactive_supervised_granular_still_prompt() {
    assert!(!clamp_bypasses_permission_prompt(
        ApprovalPolicy::Interactive
    ));
    assert!(!clamp_bypasses_permission_prompt(
        ApprovalPolicy::Supervised
    ));
    assert!(!clamp_bypasses_permission_prompt(ApprovalPolicy::Granular(
        GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            request_permissions: true,
            mcp_elicitations: true,
        }
    )));
}

#[test]
fn chaos_mcp_bridge_tools_are_allowed_at_claude_permission_layer() {
    assert!(is_clamp_mcp_tool("mcp__chaos__git_repo"));
    assert_eq!(
        clamp_tool_permission_decision(
            "mcp__chaos__git_repo",
            &serde_json::json!({}),
            Path::new("/tmp"),
            &VfsPolicy::default(),
        ),
        ClampToolPermissionDecision::Allow
    );
}

#[test]
fn non_bridge_mcp_tools_are_not_implicitly_allowed() {
    assert!(!is_clamp_mcp_tool("mcp__other__git_repo"));
    assert!(matches!(
        clamp_tool_permission_decision(
            "mcp__other__git_repo",
            &serde_json::json!({}),
            Path::new("/tmp"),
            &VfsPolicy::default(),
        ),
        ClampToolPermissionDecision::Deny(_)
    ));
}

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        end_turn: None,
        phase: None,
    }
}

fn mcp_notification_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "system".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        end_turn: None,
        phase: None,
    }
}

fn plain_system_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "system".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        end_turn: None,
        phase: None,
    }
}

fn make_prompt(input: Vec<ResponseItem>) -> Prompt {
    Prompt {
        input,
        ..Default::default()
    }
}

const NOTIFICATION: &str = "<mcp_resource_update>\nserver: \"coordinator\"\n</mcp_resource_update>";

#[test]
fn clamp_user_content_does_not_demote_base_instructions() {
    let mut prompt = make_prompt(vec![
        user_message("first turn"),
        user_message("latest turn"),
    ]);
    prompt.base_instructions.text = "Canonical system instructions.".to_string();
    for rendered in [
        render_clamp_full_prompt(&prompt),
        render_latest_clamp_user_message(&prompt),
    ] {
        assert!(!rendered.contains(&prompt.base_instructions.text));
        assert!(rendered.contains("latest turn"));
    }
}

#[test]
fn notification_after_user_message_is_included() {
    let prompt = make_prompt(vec![
        user_message("do something"),
        mcp_notification_item(NOTIFICATION),
    ]);

    let rendered = render_latest_clamp_user_message(&prompt);

    assert!(
        rendered.contains("do something"),
        "latest user message must be present"
    );
    assert!(
        rendered.contains(NOTIFICATION),
        "notification after latest user must be included"
    );
}

#[test]
fn notification_before_latest_user_message_is_excluded() {
    let prompt = make_prompt(vec![
        user_message("first turn"),
        mcp_notification_item(NOTIFICATION),
        user_message("second turn"),
    ]);

    let rendered = render_latest_clamp_user_message(&prompt);
    assert!(
        !rendered.contains(NOTIFICATION),
        "stale notification must not be re-injected"
    );
}

#[test]
fn only_mcp_notifications_are_forwarded_not_other_system_messages() {
    let prompt = make_prompt(vec![
        user_message("do something"),
        plain_system_item("unrelated system warning"),
        user_message("latest turn"),
    ]);

    let rendered = render_latest_clamp_user_message(&prompt);
    assert!(
        !rendered.contains("unrelated system warning"),
        "non-MCP system messages must not leak"
    );
}

#[test]
fn no_user_message_falls_back_to_full_prompt() {
    let prompt = make_prompt(vec![mcp_notification_item(NOTIFICATION)]);

    let rendered = render_latest_clamp_user_message(&prompt);

    assert!(
        rendered.contains("conversation_state"),
        "must fall back to render_clamp_full_prompt"
    );
    assert!(
        rendered.contains(NOTIFICATION),
        "notification must survive the full-prompt fallback"
    );
}
