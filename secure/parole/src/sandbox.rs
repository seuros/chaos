use std::path::Path;

use chaos_ipc::permissions::VfsAccessMode;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SocketPolicy;
use chaos_ipc::protocol::VfsPolicy;
use chaos_ipc::protocol::WritableRoot;
use chaos_realpath::AbsolutePathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfsPolicySemantics {
    pub has_full_disk_read_access: bool,
    pub has_full_disk_write_access: bool,
    pub include_platform_defaults: bool,
    pub readable_roots: Vec<AbsolutePathBuf>,
    pub writable_roots: Vec<WritableRoot>,
    pub unreadable_roots: Vec<AbsolutePathBuf>,
}

pub fn vfs_policy_from_sandbox_policy(sandbox_policy: &SandboxPolicy, cwd: &Path) -> VfsPolicy {
    VfsPolicy::from_sandbox_policy(sandbox_policy, cwd)
}

pub fn has_full_disk_read_access(policy: &VfsPolicy) -> bool {
    policy.has_full_disk_read_access()
}

pub fn has_full_disk_write_access(policy: &VfsPolicy) -> bool {
    policy.has_full_disk_write_access()
}

pub fn include_platform_defaults(policy: &VfsPolicy) -> bool {
    policy.include_platform_defaults()
}

pub fn readable_roots(policy: &VfsPolicy, cwd: &Path) -> Vec<AbsolutePathBuf> {
    policy.get_readable_roots_with_cwd(cwd)
}

pub fn writable_roots(policy: &VfsPolicy, cwd: &Path) -> Vec<WritableRoot> {
    policy.get_writable_roots_with_cwd(cwd)
}

pub fn unreadable_roots(policy: &VfsPolicy, cwd: &Path) -> Vec<AbsolutePathBuf> {
    policy.get_unreadable_roots_with_cwd(cwd)
}

pub fn resolve_access(policy: &VfsPolicy, path: &Path, cwd: &Path) -> VfsAccessMode {
    policy.resolve_access_with_cwd(path, cwd)
}

pub fn can_read_path(policy: &VfsPolicy, path: &Path, cwd: &Path) -> bool {
    policy.can_read_path_with_cwd(path, cwd)
}

pub fn can_write_path(policy: &VfsPolicy, path: &Path, cwd: &Path) -> bool {
    policy.can_write_path_with_cwd(path, cwd)
}

pub fn needs_direct_runtime_enforcement(
    policy: &VfsPolicy,
    network_policy: SocketPolicy,
    cwd: &Path,
) -> bool {
    policy.needs_direct_runtime_enforcement(network_policy, cwd)
}

pub fn vfs_policy_semantics(policy: &VfsPolicy, cwd: &Path) -> VfsPolicySemantics {
    VfsPolicySemantics {
        has_full_disk_read_access: policy.has_full_disk_read_access(),
        has_full_disk_write_access: policy.has_full_disk_write_access(),
        include_platform_defaults: policy.include_platform_defaults(),
        readable_roots: policy.get_readable_roots_with_cwd(cwd),
        writable_roots: policy.get_writable_roots_with_cwd(cwd),
        unreadable_roots: policy.get_unreadable_roots_with_cwd(cwd),
    }
}

pub fn vfs_policies_match_semantics(provided: &VfsPolicy, derived: &VfsPolicy, cwd: &Path) -> bool {
    vfs_policy_semantics(provided, cwd) == vfs_policy_semantics(derived, cwd)
}

pub fn sandbox_policies_match_semantics(
    provided: &SandboxPolicy,
    derived: &SandboxPolicy,
    cwd: &Path,
) -> bool {
    SocketPolicy::from(provided) == SocketPolicy::from(derived)
        && vfs_policies_match_semantics(
            &VfsPolicy::from_sandbox_policy(provided, cwd),
            &VfsPolicy::from_sandbox_policy(derived, cwd),
            cwd,
        )
}
