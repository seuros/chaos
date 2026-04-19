use super::*;
use crate::sandboxing::SandboxPermissions;
use chaos_ipc::protocol::GranularApprovalConfig;
use chaos_ipc::protocol::NetworkAccess;
use chaos_ipc::protocol::SandboxPolicy;
use pretty_assertions::assert_eq;

#[test]
fn external_sandbox_skips_exec_approval_on_request() {
    let sandbox_policy = SandboxPolicy::ExternalSandbox {
        network_access: NetworkAccess::Restricted,
    };
    assert_eq!(
        default_exec_approval_requirement(
            ApprovalPolicy::Interactive,
            &VfsPolicy::from(&sandbox_policy),
        ),
        ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        }
    );
}

#[test]
fn restricted_sandbox_requires_exec_approval_on_request() {
    let sandbox_policy = SandboxPolicy::new_read_only_policy();
    assert_eq!(
        default_exec_approval_requirement(
            ApprovalPolicy::Interactive,
            &VfsPolicy::from(&sandbox_policy)
        ),
        ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        }
    );
}

#[test]
fn default_exec_approval_requirement_rejects_sandbox_prompt_when_granular_disables_it() {
    let policy = ApprovalPolicy::Granular(GranularApprovalConfig {
        sandbox_approval: false,
        rules: true,
        request_permissions: true,
        mcp_elicitations: true,
    });

    let sandbox_policy = SandboxPolicy::new_read_only_policy();
    let requirement = default_exec_approval_requirement(policy, &VfsPolicy::from(&sandbox_policy));

    assert_eq!(
        requirement,
        ExecApprovalRequirement::Forbidden {
            reason: "approval policy disallowed sandbox approval prompt".to_string(),
        }
    );
}

#[test]
fn default_exec_approval_requirement_keeps_prompt_when_granular_allows_sandbox_approval() {
    let policy = ApprovalPolicy::Granular(GranularApprovalConfig {
        sandbox_approval: true,
        rules: false,
        request_permissions: true,
        mcp_elicitations: false,
    });

    let sandbox_policy = SandboxPolicy::new_read_only_policy();
    let requirement = default_exec_approval_requirement(policy, &VfsPolicy::from(&sandbox_policy));

    assert_eq!(
        requirement,
        ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        }
    );
}

#[test]
fn additional_permissions_allow_bypass_sandbox_first_attempt_when_execpolicy_skips() {
    assert_eq!(
        sandbox_override_for_first_attempt(
            SandboxPermissions::WithAdditionalPermissions,
            &ExecApprovalRequirement::Skip {
                bypass_sandbox: true,
                proposed_execpolicy_amendment: None,
            },
        ),
        SandboxOverride::BypassSandboxFirstAttempt
    );
}

#[test]
fn explicit_escalation_bypasses_sandbox_on_first_attempt() {
    assert_eq!(
        sandbox_override_for_first_attempt(
            SandboxPermissions::RequireEscalated,
            &ExecApprovalRequirement::Skip {
                bypass_sandbox: false,
                proposed_execpolicy_amendment: None,
            },
        ),
        SandboxOverride::BypassSandboxFirstAttempt
    );
}
