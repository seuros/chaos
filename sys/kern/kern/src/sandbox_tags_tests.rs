use super::sandbox_tag_for_vfs_policy;
use crate::exec::SandboxType;
use crate::safety::get_platform_sandbox;
use chaos_ipc::protocol::NetworkAccess;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::VfsPolicy;
use pretty_assertions::assert_eq;

#[test]
fn root_access_is_untagged_even_when_linux_sandbox_defaults_apply() {
    let actual = sandbox_tag_for_vfs_policy(&VfsPolicy::unrestricted());
    assert_eq!(actual, "none");
}

#[test]
fn external_sandbox_keeps_external_tag_when_linux_sandbox_defaults_apply() {
    let actual = sandbox_tag_for_vfs_policy(&VfsPolicy::from(&SandboxPolicy::ExternalSandbox {
        network_access: NetworkAccess::Enabled,
    }));
    assert_eq!(actual, "external");
}

#[test]
fn default_linux_sandbox_uses_platform_sandbox_tag() {
    let actual =
        sandbox_tag_for_vfs_policy(&VfsPolicy::from(&SandboxPolicy::new_read_only_policy()));
    let expected = get_platform_sandbox()
        .map(SandboxType::as_metric_tag)
        .unwrap_or("none");
    assert_eq!(actual, expected);
}
