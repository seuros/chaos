use super::sandbox_tag_for_vfs_policy;
use crate::safety::get_platform_sandbox;
use chaos_ipc::protocol::NetworkAccess;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::VfsPolicy;
use pretty_assertions::assert_eq;

#[test]
fn sandbox_tags_distinguish_unrestricted_external_and_platform_policies() {
    for (policy, expected) in [
        (VfsPolicy::unrestricted(), "none"),
        (
            VfsPolicy::from(&SandboxPolicy::ExternalSandbox {
                network_access: NetworkAccess::Enabled,
            }),
            "external",
        ),
        (
            VfsPolicy::from(&SandboxPolicy::new_read_only_policy()),
            get_platform_sandbox().as_metric_tag(),
        ),
    ] {
        assert_eq!(sandbox_tag_for_vfs_policy(&policy), expected, "{policy:?}");
    }
}
