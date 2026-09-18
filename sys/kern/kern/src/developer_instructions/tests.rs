use super::*;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

#[test]
fn builds_permissions_with_network_access_override() {
    let instructions = from_permissions_with_network(
        SandboxMode::WorkspaceWrite,
        NetworkAccess::Enabled,
        ApprovalPolicy::Interactive,
        &Policy::empty(),
        None,
        false,
        false,
    );

    let text = instructions.into_text();
    assert!(text.contains("Network access is enabled."));
    assert!(text.contains("How to request escalation"));
}

#[test]
fn builds_permissions_from_policy() {
    let file_system_policy = VfsPolicy::restricted(vec![chaos_ipc::permissions::VfsEntry {
        path: chaos_ipc::permissions::VfsPath::Special {
            value: chaos_ipc::permissions::VfsSpecialPath::CurrentWorkingDirectory,
        },
        access: chaos_ipc::permissions::VfsAccessMode::Write,
    }]);

    let instructions = from_policies(
        &file_system_policy,
        SocketPolicy::Enabled,
        ApprovalPolicy::Supervised,
        &Policy::empty(),
        &PathBuf::from("/tmp"),
        false,
        false,
    );
    let text = instructions.into_text();
    assert!(text.contains("Network access is enabled."));
    assert!(text.contains("`approval_policy` is `unless-trusted`"));
}

#[test]
fn granular_policy_exact_prompt_variants() {
    let text = from(
        ApprovalPolicy::Granular(GranularApprovalConfig {
            sandbox_approval: false,
            rules: true,
            request_permissions: true,
            mcp_elicitations: false,
        }),
        &Policy::empty(),
        true,
        false,
    )
    .into_text();

    assert_eq!(
        text,
        [
            granular_prompt_intro_text().to_string(),
            "These approval categories may still prompt the user when needed:\n- `rules`"
                .to_string(),
            "These approval categories are automatically rejected instead of prompting the user:\n- `sandbox_approval`\n- `mcp_elicitations`"
                .to_string(),
        ]
        .join("\n\n")
    );
}
