use crate::exec::SandboxType;
use crate::safety::get_platform_sandbox;
use chaos_ipc::permissions::SocketPolicy;
use chaos_ipc::permissions::VfsPolicy;
use chaos_ipc::permissions::VfsPolicyKind;
use chaos_parole::sandbox::has_full_disk_write_access;
use chaos_parole::sandbox::writable_roots;
use std::path::Path;

pub(crate) fn sandbox_tag_for_vfs_policy(vfs_policy: &VfsPolicy) -> &'static str {
    match vfs_policy.kind {
        VfsPolicyKind::Unrestricted => "none",
        VfsPolicyKind::ExternalSandbox => "external",
        VfsPolicyKind::Restricted => get_platform_sandbox()
            .map(SandboxType::as_metric_tag)
            .unwrap_or("none"),
    }
}

pub(crate) fn sandbox_policy_tag_for_policies(
    vfs_policy: &VfsPolicy,
    socket_policy: SocketPolicy,
    cwd: &Path,
) -> &'static str {
    match vfs_policy.kind {
        VfsPolicyKind::ExternalSandbox => "external-sandbox",
        VfsPolicyKind::Unrestricted => "root-access",
        VfsPolicyKind::Restricted => {
            if has_full_disk_write_access(vfs_policy) {
                if socket_policy.is_enabled() {
                    "root-access"
                } else {
                    "workspace-write"
                }
            } else if writable_roots(vfs_policy, cwd).is_empty() {
                "read-only"
            } else {
                "workspace-write"
            }
        }
    }
}

#[cfg(test)]
#[path = "sandbox_tags_tests.rs"]
mod tests;
