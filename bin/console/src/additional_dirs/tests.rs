use super::add_dir_warning_message;
use chaos_ipc::protocol::NetworkAccess;
use chaos_ipc::protocol::SandboxPolicy;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

pub(crate) fn add_dir_warning_message_only_warns_for_read_only_sandbox_with_dirs() {
    let dirs = vec![PathBuf::from("/tmp/example")];
    for sandbox in [
        SandboxPolicy::new_workspace_write_policy(),
        SandboxPolicy::RootAccess,
        SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Enabled,
        },
    ] {
        assert_eq!(add_dir_warning_message(&dirs, &sandbox), None);
    }

    let read_only_dirs = vec![PathBuf::from("relative"), PathBuf::from("/abs")];
    let message = add_dir_warning_message(&read_only_dirs, &SandboxPolicy::new_read_only_policy())
        .expect("expected warning for read-only sandbox");
    assert_eq!(
        message,
        "Ignoring --add-dir (relative, /abs) because the effective sandbox mode is read-only. Switch to workspace-write or root-access to allow additional writable roots."
    );

    let dirs: Vec<PathBuf> = Vec::new();
    assert_eq!(
        add_dir_warning_message(&dirs, &SandboxPolicy::new_read_only_policy()),
        None
    );
}
