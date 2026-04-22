#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

fn landlock_supported() -> bool {
    use landlock::{ABI, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr};
    let result = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(ABI::V1));
    result.and_then(landlock::Ruleset::create).is_ok()
}

macro_rules! require_landlock {
    () => {
        if !landlock_supported() {
            eprintln!("skipping: kernel does not support Landlock");
            return;
        }
    };
}
use chaos_ipc::permissions::SocketPolicy;
use chaos_ipc::permissions::VfsAccessMode;
use chaos_ipc::permissions::VfsEntry;
use chaos_ipc::permissions::VfsPath;
use chaos_ipc::permissions::VfsPolicy;
use chaos_ipc::permissions::VfsSpecialPath;
use chaos_ipc::protocol::ReadOnlyAccess;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_kern::config::types::ShellEnvironmentPolicy;
use chaos_kern::error::ChaosErr;
use chaos_kern::error::Result;
use chaos_kern::error::SandboxErr;
use chaos_kern::exec::ExecParams;
use chaos_kern::exec::process_exec_tool_call;
use chaos_kern::exec_env::create_env;
use chaos_kern::sandboxing::SandboxPermissions;
use chaos_realpath::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::path::PathBuf;
use tempfile::NamedTempFile;

// Arm64 test environments appear to need longer timeouts.

#[cfg(not(target_arch = "aarch64"))]
const SHORT_TIMEOUT_MS: u64 = 200;
#[cfg(target_arch = "aarch64")]
const SHORT_TIMEOUT_MS: u64 = 5_000;

#[cfg(not(target_arch = "aarch64"))]
const LONG_TIMEOUT_MS: u64 = 1_000;
#[cfg(target_arch = "aarch64")]
const LONG_TIMEOUT_MS: u64 = 5_000;

#[cfg(not(target_arch = "aarch64"))]
const NETWORK_TIMEOUT_MS: u64 = 2_000;
#[cfg(target_arch = "aarch64")]
const NETWORK_TIMEOUT_MS: u64 = 10_000;

fn create_env_from_core_vars() -> HashMap<String, String> {
    let policy = ShellEnvironmentPolicy::default();
    create_env(&policy, None)
}

#[expect(clippy::print_stdout)]
async fn run_cmd(cmd: &[&str], writable_roots: &[PathBuf], timeout_ms: u64) {
    let output = run_cmd_output(cmd, writable_roots, timeout_ms).await;
    if output.exit_code != 0 {
        println!("stdout:\n{}", output.stdout.text);
        println!("stderr:\n{}", output.stderr.text);
        panic!("exit code: {}", output.exit_code);
    }
}

#[expect(clippy::expect_used)]
async fn run_cmd_output(
    cmd: &[&str],
    writable_roots: &[PathBuf],
    timeout_ms: u64,
) -> chaos_kern::exec::ExecToolCallOutput {
    run_cmd_result_with_writable_roots(cmd, writable_roots, timeout_ms, false)
        .await
        .expect("sandboxed command should execute")
}

async fn run_cmd_result_with_writable_roots(
    cmd: &[&str],
    writable_roots: &[PathBuf],
    timeout_ms: u64,
    network_access: bool,
) -> Result<chaos_kern::exec::ExecToolCallOutput> {
    let sandbox_policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: writable_roots
            .iter()
            .map(|p| AbsolutePathBuf::try_from(p.as_path()).unwrap())
            .collect(),
        read_only_access: Default::default(),
        network_access,
        // Exclude tmp-related folders from writable roots because we need a
        // folder that is writable by tests but that we intentionally disallow
        // writing to in the sandbox.
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };
    let vfs_policy = VfsPolicy::from(&sandbox_policy);
    let socket_policy = SocketPolicy::from(&sandbox_policy);
    run_cmd_result_with_policies(cmd, sandbox_policy, vfs_policy, socket_policy, timeout_ms).await
}

#[expect(clippy::expect_used)]
async fn run_cmd_result_with_policies(
    cmd: &[&str],
    _sandbox_policy: SandboxPolicy,
    vfs_policy: VfsPolicy,
    socket_policy: SocketPolicy,
    timeout_ms: u64,
) -> Result<chaos_kern::exec::ExecToolCallOutput> {
    let cwd = std::env::current_dir().expect("cwd should exist");
    let sandbox_cwd = cwd.clone();
    let params = ExecParams {
        command: cmd.iter().copied().map(str::to_owned).collect(),
        cwd,
        expiration: timeout_ms.into(),
        env: create_env_from_core_vars(),
        network: None,
        sandbox_permissions: SandboxPermissions::UseDefault,
        justification: None,
        arg0: None,
    };
    let sandbox_program = env!("CARGO_BIN_EXE_alcatraz-linux");
    let alcatraz_linux_exe = Some(PathBuf::from(sandbox_program));
    let alcatraz_macos_exe: Option<PathBuf> = None;
    let alcatraz_freebsd_exe: Option<PathBuf> = None;

    process_exec_tool_call(
        params,
        &vfs_policy,
        socket_policy,
        sandbox_cwd.as_path(),
        &alcatraz_macos_exe,
        &alcatraz_linux_exe,
        &alcatraz_freebsd_exe,
        None,
    )
    .await
}

fn expect_denied(
    result: Result<chaos_kern::exec::ExecToolCallOutput>,
    context: &str,
) -> Option<chaos_kern::exec::ExecToolCallOutput> {
    match result {
        Ok(output) => {
            assert_ne!(output.exit_code, 0, "{context}: expected nonzero exit code");
            Some(output)
        }
        Err(ChaosErr::Sandbox(SandboxErr::Denied { output, .. })) => Some(*output),
        Err(ChaosErr::Io(err)) if err.kind() == std::io::ErrorKind::InvalidInput => None,
        Err(err) => panic!("{context}: {err:?}"),
    }
}

#[tokio::test]
async fn test_root_read() {
    require_landlock!();
    run_cmd(&["ls", "-l", "/bin"], &[], SHORT_TIMEOUT_MS).await;
}

#[tokio::test]
#[should_panic]
async fn test_root_write() {
    let tmpfile = NamedTempFile::new().unwrap();
    let tmpfile_path = tmpfile.path().to_string_lossy();
    run_cmd(
        &["bash", "-lc", &format!("echo blah > {tmpfile_path}")],
        &[],
        SHORT_TIMEOUT_MS,
    )
    .await;
}

#[tokio::test]
async fn test_dev_null_write() {
    require_landlock!();
    let output = run_cmd_result_with_writable_roots(
        &["bash", "-lc", "echo blah > /dev/null"],
        &[],
        // We have seen timeouts for this test in CI, so use a generous
        // timeout until we can diagnose further.
        LONG_TIMEOUT_MS,
        true,
    )
    .await
    .expect("sandboxed command should execute");

    assert_eq!(output.exit_code, 0);
}

#[tokio::test]
async fn linux_sandbox_populates_minimal_dev_nodes() {
    require_landlock!();
    let output = run_cmd_result_with_writable_roots(
        &[
            "bash",
            "-lc",
            "for node in null zero full random urandom tty; do [ -c \"/dev/$node\" ] || { echo \"missing /dev/$node\" >&2; exit 1; }; done",
        ],
        &[],
        LONG_TIMEOUT_MS,
        true,
    )
    .await
    .expect("sandboxed command should execute");

    assert_eq!(output.exit_code, 0);
}

#[tokio::test]
async fn linux_sandbox_preserves_writable_dev_shm_bind_mount() {
    require_landlock!();
    if !std::path::Path::new("/dev/shm").exists() {
        eprintln!("skipping Linux sandbox test: /dev/shm is unavailable in this environment");
        return;
    }

    let target_file = match NamedTempFile::new_in("/dev/shm") {
        Ok(file) => file,
        Err(err) => {
            eprintln!("skipping Linux sandbox test: failed to create /dev/shm temp file: {err}");
            return;
        }
    };
    let target_path = target_file.path().to_path_buf();
    std::fs::write(&target_path, "host-before").expect("seed /dev/shm file");

    let output = run_cmd_result_with_writable_roots(
        &[
            "bash",
            "-lc",
            &format!("printf sandbox-after > {}", target_path.to_string_lossy()),
        ],
        &[PathBuf::from("/dev/shm")],
        LONG_TIMEOUT_MS,
        true,
    )
    .await
    .expect("sandboxed command should execute");

    assert_eq!(output.exit_code, 0);
    assert_eq!(
        std::fs::read_to_string(&target_path).expect("read /dev/shm file"),
        "sandbox-after"
    );
}

#[tokio::test]
async fn test_writable_root() {
    require_landlock!();
    let tmpdir = tempfile::tempdir().unwrap();
    let file_path = tmpdir.path().join("test");
    run_cmd(
        &[
            "bash",
            "-lc",
            &format!("echo blah > {}", file_path.to_string_lossy()),
        ],
        &[tmpdir.path().to_path_buf()],
        // We have seen timeouts for this test in CI, so use a generous
        // timeout until we can diagnose further.
        LONG_TIMEOUT_MS,
    )
    .await;
}

#[tokio::test]
async fn test_no_new_privs_is_enabled() {
    require_landlock!();
    let output = run_cmd_output(
        &["bash", "-lc", "grep '^NoNewPrivs:' /proc/self/status"],
        &[],
        // We have seen timeouts when running this test in CI on GitHub,
        // so we are using a generous timeout until we can diagnose further.
        LONG_TIMEOUT_MS,
    )
    .await;
    let line = output
        .stdout
        .text
        .lines()
        .find(|line| line.starts_with("NoNewPrivs:"))
        .unwrap_or("");
    assert_eq!(line.trim(), "NoNewPrivs:\t1");
}

#[tokio::test]
#[should_panic(expected = "Sandbox(Timeout")]
async fn test_timeout() {
    run_cmd(&["sleep", "2"], &[], 50).await;
}

/// Helper that runs `cmd` under the Linux sandbox and asserts that the command
/// does NOT succeed (i.e. returns a non‑zero exit code) **unless** the binary
/// is missing in which case we silently treat it as an accepted skip so the
/// suite remains green on leaner CI images.
#[expect(clippy::expect_used)]
async fn assert_network_blocked(cmd: &[&str]) {
    let cwd = std::env::current_dir().expect("cwd should exist");
    let sandbox_cwd = cwd.clone();
    let params = ExecParams {
        command: cmd.iter().copied().map(str::to_owned).collect(),
        cwd,
        // Give the tool a generous 2-second timeout so even slow DNS timeouts
        // do not stall the suite.
        expiration: NETWORK_TIMEOUT_MS.into(),
        env: create_env_from_core_vars(),
        network: None,
        sandbox_permissions: SandboxPermissions::UseDefault,
        justification: None,
        arg0: None,
    };

    let sandbox_policy = SandboxPolicy::new_read_only_policy();
    let sandbox_program = env!("CARGO_BIN_EXE_alcatraz-linux");
    let alcatraz_linux_exe: Option<PathBuf> = Some(PathBuf::from(sandbox_program));
    let alcatraz_macos_exe: Option<PathBuf> = None;
    let alcatraz_freebsd_exe: Option<PathBuf> = None;
    let result = process_exec_tool_call(
        params,
        &VfsPolicy::from(&sandbox_policy),
        SocketPolicy::from(&sandbox_policy),
        sandbox_cwd.as_path(),
        &alcatraz_macos_exe,
        &alcatraz_linux_exe,
        &alcatraz_freebsd_exe,
        None,
    )
    .await;

    let output = match result {
        Ok(output) => output,
        Err(ChaosErr::Sandbox(SandboxErr::Denied { output, .. })) => *output,
        _ => {
            panic!("expected sandbox denied error, got: {result:?}");
        }
    };

    dbg!(&output.stderr.text);
    dbg!(&output.stdout.text);
    dbg!(&output.exit_code);

    // A completely missing binary exits with 127.  Anything else should also
    // be non‑zero (EPERM from seccomp will usually bubble up as 1, 2, 13…)
    // If—*and only if*—the command exits 0 we consider the sandbox breached.

    if output.exit_code == 0 {
        panic!(
            "Network sandbox FAILED - {cmd:?} exited 0\nstdout:\n{}\nstderr:\n{}",
            output.stdout.text, output.stderr.text
        );
    }
}

#[tokio::test]
async fn sandbox_blocks_curl() {
    require_landlock!();
    assert_network_blocked(&["curl", "-I", "http://openai.com"]).await;
}

#[tokio::test]
async fn sandbox_blocks_wget() {
    require_landlock!();
    assert_network_blocked(&["wget", "-qO-", "http://openai.com"]).await;
}

#[tokio::test]
async fn sandbox_blocks_ping() {
    require_landlock!();
    // ICMP requires raw socket – should be denied quickly with EPERM.
    assert_network_blocked(&["ping", "-c", "1", "8.8.8.8"]).await;
}

#[tokio::test]
async fn sandbox_blocks_nc() {
    require_landlock!();
    // Zero‑length connection attempt to localhost.
    assert_network_blocked(&["nc", "-z", "127.0.0.1", "80"]).await;
}

#[tokio::test]
async fn workspace_write_currently_allows_git_and_chaos_writes_on_linux_landlock_backend() {
    require_landlock!();
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let dot_git = tmpdir.path().join(".git");
    let dot_chaos = tmpdir.path().join(".chaos");
    std::fs::create_dir_all(&dot_git).expect("create .git");
    std::fs::create_dir_all(&dot_chaos).expect("create .chaos");

    let git_target = dot_git.join("config");
    let chaos_target = dot_chaos.join("config.toml");

    let git_output = run_cmd_result_with_writable_roots(
        &[
            "bash",
            "-lc",
            &format!("echo allowed > {}", git_target.to_string_lossy()),
        ],
        &[tmpdir.path().to_path_buf()],
        LONG_TIMEOUT_MS,
        true,
    )
    .await
    .expect("workspace-write should execute even though .git is not specially protected");

    let chaos_output = run_cmd_result_with_writable_roots(
        &[
            "bash",
            "-lc",
            &format!("echo allowed > {}", chaos_target.to_string_lossy()),
        ],
        &[tmpdir.path().to_path_buf()],
        LONG_TIMEOUT_MS,
        true,
    )
    .await
    .expect("workspace-write should execute even though .chaos is not specially protected");

    assert_eq!(git_output.exit_code, 0);
    assert_eq!(chaos_output.exit_code, 0);
}

#[tokio::test]
async fn workspace_write_currently_allows_chaos_symlink_replacement_on_linux_landlock_backend() {
    require_landlock!();
    use std::os::unix::fs::symlink;

    let tmpdir = tempfile::tempdir().expect("tempdir");
    let decoy = tmpdir.path().join("decoy-chaos");
    std::fs::create_dir_all(&decoy).expect("create decoy dir");

    let dot_chaos = tmpdir.path().join(".chaos");
    symlink(&decoy, &dot_chaos).expect("create .chaos symlink");

    let chaos_target = dot_chaos.join("config.toml");

    let chaos_output = run_cmd_result_with_writable_roots(
        &[
            "bash",
            "-lc",
            &format!("echo allowed > {}", chaos_target.to_string_lossy()),
        ],
        &[tmpdir.path().to_path_buf()],
        LONG_TIMEOUT_MS,
        true,
    )
    .await
    .expect(
        "workspace-write should execute even though symlinked .chaos is not specially protected",
    );
    assert_eq!(chaos_output.exit_code, 0);
}

#[tokio::test]
async fn linux_landlock_rejects_explicit_split_policy_carveouts() {
    require_landlock!();
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let blocked = tmpdir.path().join("blocked");
    std::fs::create_dir_all(&blocked).expect("create blocked dir");
    let blocked_target = blocked.join("secret.txt");
    // These tests bypass the usual legacy-policy bridge, so explicitly keep
    // the sandbox helper binary and minimal runtime paths readable.
    let sandbox_helper_dir = PathBuf::from(env!("CARGO_BIN_EXE_alcatraz-linux"))
        .parent()
        .expect("sandbox helper should have a parent")
        .to_path_buf();

    let sandbox_policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![AbsolutePathBuf::try_from(tmpdir.path()).expect("absolute tempdir")],
        read_only_access: Default::default(),
        network_access: true,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };
    let vfs_policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::Minimal,
            },
            access: VfsAccessMode::Read,
        },
        VfsEntry {
            path: VfsPath::Path {
                path: AbsolutePathBuf::try_from(sandbox_helper_dir.as_path())
                    .expect("absolute helper dir"),
            },
            access: VfsAccessMode::Read,
        },
        VfsEntry {
            path: VfsPath::Path {
                path: AbsolutePathBuf::try_from(tmpdir.path()).expect("absolute tempdir"),
            },
            access: VfsAccessMode::Write,
        },
        VfsEntry {
            path: VfsPath::Path {
                path: AbsolutePathBuf::try_from(blocked.as_path()).expect("absolute blocked dir"),
            },
            access: VfsAccessMode::None,
        },
    ]);
    let output = expect_denied(
        run_cmd_result_with_policies(
            &[
                "bash",
                "-lc",
                &format!("echo denied > {}", blocked_target.to_string_lossy()),
            ],
            sandbox_policy,
            vfs_policy,
            SocketPolicy::Enabled,
            LONG_TIMEOUT_MS,
        )
        .await,
        "explicit split-policy carveout should be rejected by the Linux landlock backend",
    );

    if let Some(output) = output {
        assert_ne!(output.exit_code, 0);
    }
}

#[tokio::test]
async fn linux_landlock_rejects_nested_writable_carveouts_inside_unreadable_parents() {
    require_landlock!();
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let blocked = tmpdir.path().join("blocked");
    let allowed = blocked.join("allowed");
    std::fs::create_dir_all(&allowed).expect("create blocked/allowed dir");
    let allowed_target = allowed.join("note.txt");
    // These tests bypass the usual legacy-policy bridge, so explicitly keep
    // the sandbox helper binary and minimal runtime paths readable.
    let sandbox_helper_dir = PathBuf::from(env!("CARGO_BIN_EXE_alcatraz-linux"))
        .parent()
        .expect("sandbox helper should have a parent")
        .to_path_buf();

    let sandbox_policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![AbsolutePathBuf::try_from(tmpdir.path()).expect("absolute tempdir")],
        read_only_access: Default::default(),
        network_access: true,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };
    let vfs_policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::Minimal,
            },
            access: VfsAccessMode::Read,
        },
        VfsEntry {
            path: VfsPath::Path {
                path: AbsolutePathBuf::try_from(sandbox_helper_dir.as_path())
                    .expect("absolute helper dir"),
            },
            access: VfsAccessMode::Read,
        },
        VfsEntry {
            path: VfsPath::Path {
                path: AbsolutePathBuf::try_from(tmpdir.path()).expect("absolute tempdir"),
            },
            access: VfsAccessMode::Write,
        },
        VfsEntry {
            path: VfsPath::Path {
                path: AbsolutePathBuf::try_from(blocked.as_path()).expect("absolute blocked dir"),
            },
            access: VfsAccessMode::None,
        },
        VfsEntry {
            path: VfsPath::Path {
                path: AbsolutePathBuf::try_from(allowed.as_path()).expect("absolute allowed dir"),
            },
            access: VfsAccessMode::Write,
        },
    ]);
    let output = expect_denied(
        run_cmd_result_with_policies(
            &[
                "bash",
                "-lc",
                &format!(
                    "printf allowed > {} && cat {}",
                    allowed_target.to_string_lossy(),
                    allowed_target.to_string_lossy()
                ),
            ],
            sandbox_policy,
            vfs_policy,
            SocketPolicy::Enabled,
            LONG_TIMEOUT_MS,
        )
        .await,
        "nested writable carveout should be rejected by the Linux landlock backend",
    );

    if let Some(output) = output {
        assert_ne!(output.exit_code, 0);
    }
}

#[tokio::test]
async fn linux_landlock_rejects_root_read_carveouts() {
    require_landlock!();
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let blocked = tmpdir.path().join("blocked");
    std::fs::create_dir_all(&blocked).expect("create blocked dir");
    let blocked_target = blocked.join("secret.txt");
    std::fs::write(&blocked_target, "secret").expect("seed blocked file");

    let sandbox_policy = SandboxPolicy::ReadOnly {
        access: ReadOnlyAccess::FullAccess,
        network_access: true,
    };
    let vfs_policy = VfsPolicy::restricted(vec![
        VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::Root,
            },
            access: VfsAccessMode::Read,
        },
        VfsEntry {
            path: VfsPath::Path {
                path: AbsolutePathBuf::try_from(blocked.as_path()).expect("absolute blocked dir"),
            },
            access: VfsAccessMode::None,
        },
    ]);
    let output = expect_denied(
        run_cmd_result_with_policies(
            &[
                "bash",
                "-lc",
                &format!("cat {}", blocked_target.to_string_lossy()),
            ],
            sandbox_policy,
            vfs_policy,
            SocketPolicy::Enabled,
            LONG_TIMEOUT_MS,
        )
        .await,
        "root-read carveout should be rejected by the Linux landlock backend",
    );

    if let Some(output) = output {
        assert_ne!(output.exit_code, 0);
    }
}

#[tokio::test]
async fn sandbox_blocks_ssh() {
    require_landlock!();
    // Force ssh to attempt a real TCP connection but fail quickly.  `BatchMode`
    // avoids password prompts, and `ConnectTimeout` keeps the hang time low.
    assert_network_blocked(&[
        "ssh",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=1",
        "github.com",
    ])
    .await;
}

#[tokio::test]
async fn sandbox_blocks_getent() {
    require_landlock!();
    assert_network_blocked(&["getent", "ahosts", "openai.com"]).await;
}

#[tokio::test]
async fn sandbox_blocks_dev_tcp_redirection() {
    require_landlock!();
    // This syntax is only supported by bash and zsh. We try bash first.
    // Fallback generic socket attempt using /bin/sh with bash‑style /dev/tcp.  Not
    // all images ship bash, so we guard against 127 as well.
    assert_network_blocked(&["bash", "-c", "echo hi > /dev/tcp/127.0.0.1/80"]).await;
}
