//! Public-API tests for `chaos-jail-report` — turning a sandbox policy
//! into a line humans can read without misreading.
//!
//! `summarize_sandbox_policy` is a formatter with real blast radius:
//! every CI log, every status line, every "why did my turn fail" hint
//! goes through it. One dense pass walks every policy variant and the
//! network-access suffix rule so a copy-paste refactor can't quietly
//! rename a mode or drop the warning.

use chaos_ipc::protocol::NetworkAccess;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_jail_report::summarize_sandbox_policy;
use chaos_realpath::AbsolutePathBuf;
use pretty_assertions::assert_eq;

#[test]
fn sandbox_policy_summary_covers_every_variant_and_network_suffix() {
    // ExternalSandbox: restricted network is silent, enabled network
    // earns the explicit warning suffix.
    assert_eq!(
        summarize_sandbox_policy(&SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Restricted,
        }),
        "external-sandbox"
    );
    assert_eq!(
        summarize_sandbox_policy(&SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Enabled,
        }),
        "external-sandbox (network access enabled)"
    );

    // ReadOnly with network access must also surface the suffix so an
    // operator can't miss it in a log scan.
    assert_eq!(
        summarize_sandbox_policy(&SandboxPolicy::ReadOnly {
            access: Default::default(),
            network_access: true,
        }),
        "read-only (network access enabled)"
    );

    // WorkspaceWrite renders the writable-roots list after the mode
    // name and still appends the network suffix when enabled. Tmpdir
    // entries are excluded here so the list is predictable.
    let writable_root = AbsolutePathBuf::try_from("/repo").expect("absolute");
    let workspace = summarize_sandbox_policy(&SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![writable_root.clone()],
        read_only_access: Default::default(),
        network_access: true,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    });
    assert_eq!(
        workspace,
        format!(
            "workspace-write [workdir, {}] (network access enabled)",
            writable_root.to_string_lossy()
        )
    );
}
