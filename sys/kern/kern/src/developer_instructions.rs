use std::path::Path;

use chaos_ipc::config_types::SandboxMode;
use chaos_ipc::models::DeveloperInstructions;
use chaos_ipc::models::format_allow_prefixes;
use chaos_ipc::permissions::SocketPolicy;
use chaos_ipc::permissions::VfsPolicy;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::GranularApprovalConfig;
use chaos_ipc::protocol::NetworkAccess;
use chaos_ipc::protocol::WritableRoot;
use chaos_parole::sandbox::has_full_disk_write_access;
use chaos_parole::sandbox::writable_roots;
use chaos_selinux::Policy;

const APPROVAL_POLICY_NEVER: &str = include_str!(
    "../../../../lib/libcontract/ipc/src/prompts/permissions/approval_policy/never.md"
);
const APPROVAL_POLICY_UNLESS_TRUSTED: &str = include_str!(
    "../../../../lib/libcontract/ipc/src/prompts/permissions/approval_policy/unless_trusted.md"
);
const APPROVAL_POLICY_ON_REQUEST_RULE: &str = include_str!(
    "../../../../lib/libcontract/ipc/src/prompts/permissions/approval_policy/on_request_rule.md"
);
const APPROVAL_POLICY_ON_REQUEST_RULE_REQUEST_PERMISSION: &str = include_str!(
    "../../../../lib/libcontract/ipc/src/prompts/permissions/approval_policy/on_request_rule_request_permission.md"
);

const SANDBOX_MODE_ROOT_ACCESS: &str = include_str!(
    "../../../../lib/libcontract/ipc/src/prompts/permissions/sandbox_mode/root_access.md"
);
const SANDBOX_MODE_WORKSPACE_WRITE: &str = include_str!(
    "../../../../lib/libcontract/ipc/src/prompts/permissions/sandbox_mode/workspace_write.md"
);
const SANDBOX_MODE_READ_ONLY: &str = include_str!(
    "../../../../lib/libcontract/ipc/src/prompts/permissions/sandbox_mode/read_only.md"
);

pub(crate) fn from(
    approval_policy: ApprovalPolicy,
    exec_policy: &Policy,
    exec_permission_approvals_enabled: bool,
    request_permissions_tool_enabled: bool,
) -> DeveloperInstructions {
    let with_request_permissions_tool = |text: &str| {
        if request_permissions_tool_enabled {
            format!("{text}\n\n{}", request_permissions_tool_prompt_section())
        } else {
            text.to_string()
        }
    };
    let on_request_instructions = || {
        let on_request_rule = if exec_permission_approvals_enabled {
            APPROVAL_POLICY_ON_REQUEST_RULE_REQUEST_PERMISSION.to_string()
        } else {
            APPROVAL_POLICY_ON_REQUEST_RULE.to_string()
        };
        let mut sections = vec![on_request_rule];
        if request_permissions_tool_enabled {
            sections.push(request_permissions_tool_prompt_section().to_string());
        }
        if let Some(prefixes) = approved_command_prefixes_text(exec_policy) {
            sections.push(format!(
                "## Approved command prefixes\nThe following prefix rules have already been approved: {prefixes}"
            ));
        }
        sections.join("\n\n")
    };
    let text = match approval_policy {
        ApprovalPolicy::Headless => APPROVAL_POLICY_NEVER.to_string(),
        ApprovalPolicy::Supervised => with_request_permissions_tool(APPROVAL_POLICY_UNLESS_TRUSTED),
        ApprovalPolicy::Interactive => on_request_instructions(),
        ApprovalPolicy::Granular(granular_config) => granular_instructions(
            granular_config,
            exec_policy,
            exec_permission_approvals_enabled,
            request_permissions_tool_enabled,
        ),
    };

    DeveloperInstructions::new(text)
}

pub(crate) fn from_policies(
    vfs_policy: &VfsPolicy,
    socket_policy: SocketPolicy,
    approval_policy: ApprovalPolicy,
    exec_policy: &Policy,
    cwd: &Path,
    exec_permission_approvals_enabled: bool,
    request_permissions_tool_enabled: bool,
) -> DeveloperInstructions {
    let network_access = if socket_policy.is_enabled() {
        NetworkAccess::Enabled
    } else {
        NetworkAccess::Restricted
    };

    let (sandbox_mode, writable_roots) = match vfs_policy.kind {
        chaos_ipc::permissions::VfsPolicyKind::Unrestricted => (SandboxMode::RootAccess, None),
        chaos_ipc::permissions::VfsPolicyKind::ExternalSandbox => (SandboxMode::RootAccess, None),
        chaos_ipc::permissions::VfsPolicyKind::Restricted => {
            if has_full_disk_write_access(vfs_policy) {
                (SandboxMode::RootAccess, None)
            } else {
                let roots = writable_roots(vfs_policy, cwd);
                if roots.is_empty() {
                    (SandboxMode::ReadOnly, None)
                } else {
                    (SandboxMode::WorkspaceWrite, Some(roots))
                }
            }
        }
    };

    from_permissions_with_network(
        sandbox_mode,
        network_access,
        approval_policy,
        exec_policy,
        writable_roots,
        exec_permission_approvals_enabled,
        request_permissions_tool_enabled,
    )
}

pub(crate) fn from_permissions_with_network(
    sandbox_mode: SandboxMode,
    network_access: NetworkAccess,
    approval_policy: ApprovalPolicy,
    exec_policy: &Policy,
    writable_roots: Option<Vec<WritableRoot>>,
    exec_permission_approvals_enabled: bool,
    request_permissions_tool_enabled: bool,
) -> DeveloperInstructions {
    let start_tag = DeveloperInstructions::new("<permissions instructions>");
    let end_tag = DeveloperInstructions::new("</permissions instructions>");
    start_tag
        .concat(sandbox_text(sandbox_mode, network_access))
        .concat(from(
            approval_policy,
            exec_policy,
            exec_permission_approvals_enabled,
            request_permissions_tool_enabled,
        ))
        .concat(from_writable_roots(writable_roots))
        .concat(end_tag)
}

fn from_writable_roots(writable_roots: Option<Vec<WritableRoot>>) -> DeveloperInstructions {
    let Some(roots) = writable_roots else {
        return DeveloperInstructions::new("");
    };

    if roots.is_empty() {
        return DeveloperInstructions::new("");
    }

    let roots_list: Vec<String> = roots
        .iter()
        .map(|r| format!("`{}`", r.root.to_string_lossy()))
        .collect();
    let text = if roots_list.len() == 1 {
        format!(" The writable root is {}.", roots_list[0])
    } else {
        format!(" The writable roots are {}.", roots_list.join(", "))
    };
    DeveloperInstructions::new(text)
}

fn sandbox_text(mode: SandboxMode, network_access: NetworkAccess) -> DeveloperInstructions {
    let template = match mode {
        SandboxMode::RootAccess => SANDBOX_MODE_ROOT_ACCESS.trim_end(),
        SandboxMode::WorkspaceWrite => SANDBOX_MODE_WORKSPACE_WRITE.trim_end(),
        SandboxMode::ReadOnly => SANDBOX_MODE_READ_ONLY.trim_end(),
    };
    let text = template.replace("{network_access}", &network_access.to_string());

    DeveloperInstructions::new(text)
}

fn approved_command_prefixes_text(exec_policy: &Policy) -> Option<String> {
    format_allow_prefixes(exec_policy.get_allowed_prefixes())
        .filter(|prefixes| !prefixes.is_empty())
}

fn granular_prompt_intro_text() -> &'static str {
    "# Approval Requests\n\nApproval policy is `granular`. Categories set to `false` are automatically rejected instead of prompting the user."
}

fn request_permissions_tool_prompt_section() -> &'static str {
    "# request_permissions Tool\n\nThe built-in `request_permissions` tool is available in this session. Invoke it when you need to request additional `network`, `file_system`, or `macos` permissions before later shell-like commands need them. Request only the specific permissions required for the task."
}

fn granular_instructions(
    granular_config: GranularApprovalConfig,
    exec_policy: &Policy,
    exec_permission_approvals_enabled: bool,
    request_permissions_tool_enabled: bool,
) -> String {
    let sandbox_approval_prompts_allowed = granular_config.allows_sandbox_approval();
    let shell_permission_requests_available =
        exec_permission_approvals_enabled && sandbox_approval_prompts_allowed;
    let request_permissions_tool_prompts_allowed =
        request_permissions_tool_enabled && granular_config.allows_request_permissions();
    let categories = [
        Some((
            granular_config.allows_sandbox_approval(),
            "`sandbox_approval`",
        )),
        Some((granular_config.allows_rules_approval(), "`rules`")),
        request_permissions_tool_enabled.then_some((
            granular_config.allows_request_permissions(),
            "`request_permissions`",
        )),
        Some((
            granular_config.allows_mcp_elicitations(),
            "`mcp_elicitations`",
        )),
    ];
    let prompted_categories = categories
        .iter()
        .flatten()
        .filter(|&&(is_allowed, _)| is_allowed)
        .map(|&(_, category)| format!("- {category}"))
        .collect::<Vec<_>>();
    let rejected_categories = categories
        .iter()
        .flatten()
        .filter(|&&(is_allowed, _)| !is_allowed)
        .map(|&(_, category)| format!("- {category}"))
        .collect::<Vec<_>>();

    let mut sections = vec![granular_prompt_intro_text().to_string()];

    if !prompted_categories.is_empty() {
        sections.push(format!(
            "These approval categories may still prompt the user when needed:\n{}",
            prompted_categories.join("\n")
        ));
    }
    if !rejected_categories.is_empty() {
        sections.push(format!(
            "These approval categories are automatically rejected instead of prompting the user:\n{}",
            rejected_categories.join("\n")
        ));
    }

    if shell_permission_requests_available {
        sections.push(APPROVAL_POLICY_ON_REQUEST_RULE_REQUEST_PERMISSION.to_string());
    }

    if request_permissions_tool_prompts_allowed {
        sections.push(request_permissions_tool_prompt_section().to_string());
    }

    if let Some(prefixes) = approved_command_prefixes_text(exec_policy) {
        sections.push(format!(
            "## Approved command prefixes\nThe following prefix rules have already been approved: {prefixes}"
        ));
    }

    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
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
}
