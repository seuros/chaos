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
