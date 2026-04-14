use super::*;
use chaos_ipc::protocol::GranularApprovalConfig;

#[test]
fn wants_no_sandbox_approval_granular_respects_sandbox_flag() {
    let runtime = ApplyPatchRuntime::new();
    assert!(runtime.wants_no_sandbox_approval(ApprovalPolicy::Interactive));
    assert!(
        !runtime.wants_no_sandbox_approval(ApprovalPolicy::Granular(GranularApprovalConfig {
            sandbox_approval: false,
            rules: true,
            request_permissions: true,
            mcp_elicitations: true,
        }))
    );
    assert!(
        runtime.wants_no_sandbox_approval(ApprovalPolicy::Granular(GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            request_permissions: true,
            mcp_elicitations: true,
        }))
    );
}
