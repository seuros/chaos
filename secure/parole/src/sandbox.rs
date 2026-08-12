use std::path::Path;

use chaos_ipc::permissions::VfsAccessMode;
use chaos_ipc::permissions::VfsSemanticSignature;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SocketPolicy;
use chaos_ipc::protocol::VfsPolicy;
use chaos_ipc::protocol::WritableRoot;
use chaos_realpath::AbsolutePathBuf;

/// Order-insensitive snapshot of effective VFS grants.
///
/// Thin alias over the shared chaos-ipc semantic signature so callers keep a
/// stable parole-facing name while normalization lives in one place.
pub type VfsPolicySemantics = VfsSemanticSignature;

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
    policy.semantic_signature(cwd)
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
