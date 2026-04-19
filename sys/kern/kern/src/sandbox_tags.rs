use crate::exec::SandboxType;
use crate::safety::get_platform_sandbox;
use chaos_ipc::permissions::FileSystemSandboxKind;
use chaos_ipc::permissions::FileSystemSandboxPolicy;
use chaos_ipc::permissions::NetworkSandboxPolicy;
use chaos_parole::sandbox::has_full_disk_write_access;
use chaos_parole::sandbox::writable_roots;
use std::path::Path;

pub(crate) fn sandbox_tag_for_file_system_policy(
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
) -> &'static str {
    match file_system_sandbox_policy.kind {
        FileSystemSandboxKind::Unrestricted => "none",
        FileSystemSandboxKind::ExternalSandbox => "external",
        FileSystemSandboxKind::Restricted => get_platform_sandbox()
            .map(SandboxType::as_metric_tag)
            .unwrap_or("none"),
    }
}

pub(crate) fn sandbox_policy_tag_for_policies(
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    network_sandbox_policy: NetworkSandboxPolicy,
    cwd: &Path,
) -> &'static str {
    match file_system_sandbox_policy.kind {
        FileSystemSandboxKind::ExternalSandbox => "external-sandbox",
        FileSystemSandboxKind::Unrestricted => "root-access",
        FileSystemSandboxKind::Restricted => {
            if has_full_disk_write_access(file_system_sandbox_policy) {
                if network_sandbox_policy.is_enabled() {
                    "root-access"
                } else {
                    "workspace-write"
                }
            } else if writable_roots(file_system_sandbox_policy, cwd).is_empty() {
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
