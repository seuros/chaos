use super::EffectiveAdditionalPermissions;
use super::implicit_granted_permissions;
use super::normalize_and_validate_additional_permissions;
use crate::sandboxing::SandboxPermissions;
use chaos_ipc::models::FileSystemPermissions;
use chaos_ipc::models::NetworkPermissions;
use chaos_ipc::models::PermissionProfile;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::GranularApprovalConfig;
use chaos_realpath::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

fn network_permissions() -> PermissionProfile {
    PermissionProfile {
        network: Some(NetworkPermissions {
            enabled: Some(true),
        }),
        ..Default::default()
    }
}

fn file_system_permissions(path: &std::path::Path) -> PermissionProfile {
    PermissionProfile {
        file_system: Some(FileSystemPermissions {
            read: None,
            write: Some(vec![
                AbsolutePathBuf::from_absolute_path(path).expect("absolute path"),
            ]),
        }),
        ..Default::default()
    }
}

#[test]
fn preapproved_permissions_work_when_request_permissions_tool_is_enabled_without_inline_exec_approval()
 {
    let cwd = tempdir().expect("tempdir");

    let normalized = normalize_and_validate_additional_permissions(
        false,
        ApprovalPolicy::Granular(GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            request_permissions: false,
            mcp_elicitations: true,
        }),
        SandboxPermissions::WithAdditionalPermissions,
        Some(network_permissions()),
        true,
        cwd.path(),
    )
    .expect("preapproved permissions should be allowed");

    assert_eq!(normalized, Some(network_permissions()));
}

#[test]
fn fresh_additional_permissions_still_require_sandbox_approval_policy() {
    let cwd = tempdir().expect("tempdir");

    let err = normalize_and_validate_additional_permissions(
        false,
        ApprovalPolicy::Interactive,
        SandboxPermissions::WithAdditionalPermissions,
        Some(network_permissions()),
        false,
        cwd.path(),
    )
    .expect_err("fresh inline permission requests should remain disabled");

    assert_eq!(
        err,
        "additional permissions are disabled by the current approval policy; switch to an approval policy that allows sandbox approval before using `with_additional_permissions`"
    );
}

#[test]
fn implicit_sticky_grants_bypass_inline_permission_validation() {
    let cwd = tempdir().expect("tempdir");
    let granted_permissions = file_system_permissions(cwd.path());
    let implicit_permissions = implicit_granted_permissions(
        SandboxPermissions::UseDefault,
        None,
        &EffectiveAdditionalPermissions {
            sandbox_permissions: SandboxPermissions::WithAdditionalPermissions,
            additional_permissions: Some(granted_permissions.clone()),
            permissions_preapproved: false,
        },
    );

    assert_eq!(implicit_permissions, Some(granted_permissions));
}

#[test]
fn explicit_inline_permissions_do_not_use_implicit_sticky_grant_path() {
    let cwd = tempdir().expect("tempdir");
    let requested_permissions = file_system_permissions(cwd.path());
    let implicit_permissions = implicit_granted_permissions(
        SandboxPermissions::WithAdditionalPermissions,
        Some(&requested_permissions),
        &EffectiveAdditionalPermissions {
            sandbox_permissions: SandboxPermissions::WithAdditionalPermissions,
            additional_permissions: Some(requested_permissions.clone()),
            permissions_preapproved: false,
        },
    );

    assert_eq!(implicit_permissions, None);
}
