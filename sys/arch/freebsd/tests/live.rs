#![cfg(target_os = "freebsd")]
//! Explicit live tests: build `alcatraz`, load the ABI-1 module, then run:
//! CHAOS_FREEBSD_TEST_HELPER=/absolute/path/to/alcatraz \
//! cargo test -p alcatraz-freebsd --test live \
//! live_network_process_driver_without_filesystem_restrictions -- --ignored --nocapture
//! No executable workspace-write gate exists until Warden can enforce it.

use chaos_ipc::protocol::{SandboxPolicy, SocketPolicy, VfsPolicy};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn run_c_fixture(name: &str) -> std::io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temp = tempfile::tempdir()?;
    let test = temp.path().join(name);
    assert!(
        Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror", "-I"])
            .arg(root.join("kmod"))
            .arg(root.join(format!("tests/fixtures/{name}.c")))
            .arg("-o")
            .arg(&test)
            .status()?
            .success(),
        "failed to compile {name}"
    );
    assert!(
        Command::new(&test).status()?.success(),
        "{name} regression failed"
    );
    Ok(())
}

#[test]
fn late_load_null_label_accessor_regression_without_kernel_module() -> std::io::Result<()> {
    run_c_fixture("label_slot")
}

#[test]
fn parser_rejects_unsupported_filesystem_claims_without_kernel_module() -> std::io::Result<()> {
    run_c_fixture("policy_text")
}

#[test]
fn production_driver_does_not_register_filesystem_hooks() {
    let source = include_str!("../kmod/mac_gulag.c");
    assert!(!source.contains(".mpo_vnode_"));
    assert!(!source.contains("vn_fullpath"));
    assert!(!source.contains("path_resolver.h"));
    assert!(source.contains("gulag_abi = 1"));
}

#[test]
#[ignore = "requires a freshly built helper binary, but no module or jail"]
fn helper_refuses_filesystem_requests_before_executing_target() {
    let f = Fixture::new();
    let marker = f.root.join("must-not-execute");
    for workspace in [false, true] {
        let result = f.run(
            &["write", marker.to_str().unwrap()],
            SocketPolicy::Enabled,
            workspace,
        );
        assert!(!result.status.success(), "{result:?}");
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("Warden"),
            "{result:?}"
        );
        assert!(!marker.exists());
    }
    let unrestricted = f.run_with_vfs(
        &["hardening"],
        SocketPolicy::Enabled,
        &VfsPolicy::unrestricted(),
    );
    assert!(unrestricted.status.success(), "{unrestricted:?}");
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
    probe: PathBuf,
    helper: PathBuf,
}

#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Test-fixture setup and subprocess failures must abort the test"
)]
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let outside = temp.path().join("outside");
        let probe = temp.path().join("probe");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::create_dir(root.join(".chaos")).unwrap();
        let source = temp.path().join("probe.c");
        fs::write(&source, include_str!("fixtures/probe.c")).unwrap();
        assert!(
            Command::new("cc")
                .args(["-Wall", "-Wextra", "-Werror", "-o"])
                .arg(&probe)
                .arg(&source)
                .status()
                .unwrap()
                .success()
        );
        let helper = std::env::var_os("CHAOS_FREEBSD_TEST_HELPER")
            .map(PathBuf::from)
            .expect("set CHAOS_FREEBSD_TEST_HELPER to the freshly built alcatraz binary");
        assert!(helper.is_absolute() && helper.is_file());
        Self {
            _temp: temp,
            root,
            outside,
            probe,
            helper,
        }
    }

    fn run(&self, args: &[&str], network: SocketPolicy, workspace: bool) -> Output {
        // With --nocapture this identifies the last operation before a panic.
        eprintln!("gulag probe: {args:?}, workspace={workspace}, network={network:?}");
        let policy = if workspace {
            // Deliberately exclude /tmp: it contains this test's outside files.
            SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![],
                read_only_access: Default::default(),
                network_access: network.is_enabled(),
                exclude_tmpdir_env_var: true,
                exclude_slash_tmp: true,
            }
        } else {
            SandboxPolicy::new_read_only_policy()
        };
        let vfs = VfsPolicy::from_sandbox_policy(&policy, &self.root);
        self.run_with_vfs(args, network, &vfs)
    }

    fn run_with_vfs(&self, args: &[&str], network: SocketPolicy, vfs: &VfsPolicy) -> Output {
        eprintln!("ABI-1 probe: {args:?}, network={network:?}");
        let mut command = vec![self.probe.to_string_lossy().into_owned()];
        command.extend(args.iter().map(|s| (*s).to_owned()));
        let prepared =
            alcatraz_freebsd::prepare_command(alcatraz_base::prepared_command::SandboxRequest {
                executable: &self.helper,
                command,
                file_system_policy: vfs,
                network_policy: network,
                sandbox_policy_cwd: &self.root,
                enforce_managed_network: false,
                network: None,
                platform_permissions: None,
            })
            .unwrap();
        Command::new(prepared.program)
            .args(prepared.args)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

#[test]
#[ignore = "requires an explicitly loaded experimental ABI-1 module"]
fn live_network_process_driver_without_filesystem_restrictions() {
    let f = Fixture::new();
    let vfs = VfsPolicy::unrestricted();
    let label = f.run_with_vfs(&["label"], SocketPolicy::Restricted, &vfs);
    assert!(label.status.success());
    assert_eq!(
        String::from_utf8_lossy(&label.stdout).trim(),
        "gulag/restrict!v=1!net=deny"
    );
    let inherited = f.run_with_vfs(&["fork-exec-label"], SocketPolicy::Restricted, &vfs);
    assert!(inherited.status.success(), "{inherited:?}");
    assert_eq!(inherited.stdout, label.stdout);
    let family = f.run_with_vfs(&["signal-family"], SocketPolicy::Restricted, &vfs);
    assert!(family.status.success(), "{family:?}");
    for mode in ["tcp4", "tcp6", "udp", "socketpair", "hardening"] {
        let output = f.run_with_vfs(&[mode], SocketPolicy::Restricted, &vfs);
        assert_eq!(
            output.status.success(),
            matches!(mode, "socketpair" | "hardening"),
            "{mode}: {output:?}"
        );
    }
    let socket = f.outside.join("broker.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    let output = f.run_with_vfs(
        &["unix-connect", socket.to_str().unwrap()],
        SocketPolicy::Restricted,
        &vfs,
    );
    assert!(!output.status.success());
    let widened = f.run_with_vfs(
        &["setlabel", "gulag/restrict!v=1"],
        SocketPolicy::Restricted,
        &vfs,
    );
    assert!(!widened.status.success());
    let mut sleeper = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let pid = sleeper.id().to_string();
    let signal = f.run_with_vfs(&["signal", &pid], SocketPolicy::Restricted, &vfs);
    let trace = f.run_with_vfs(&["ptrace", &pid], SocketPolicy::Restricted, &vfs);
    let alive = sleeper.try_wait().unwrap().is_none();
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    assert!(!signal.status.success() && !trace.status.success() && alive);
    // ABI 1 deliberately permits ordinary file operations: its contract is
    // network/process policy, not filesystem confinement.
    let path = f.root.join("ordinary-output");
    let output = f.run_with_vfs(
        &["write", path.to_str().unwrap()],
        SocketPolicy::Restricted,
        &vfs,
    );
    assert!(output.status.success(), "{output:?}");
}

#[test]
#[ignore = "requires root, an explicitly loaded ABI-1 module and freshly built helper/probe"]
fn live_warden_readonly_with_production_launcher() {
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "run this explicit test as root"
    );
    let f = Fixture::new();
    let harness = PathBuf::from(
        std::env::var_os("CHAOS_FREEBSD_TEST_HARNESS_PROBE")
            .expect("set CHAOS_FREEBSD_TEST_HARNESS_PROBE"),
    );
    assert!(harness.is_absolute() && harness.is_file());
    let user = std::env::var("CHAOS_FREEBSD_TEST_USER")
        .expect("set CHAOS_FREEBSD_TEST_USER to an existing non-root account");
    // The freshly compiled root-owned probe is immutable but executable by
    // the selected user. No host permission changes outside this temp scope.
    fs::set_permissions(f._temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&f.probe, fs::Permissions::from_mode(0o755)).unwrap();
    // A fresh path directly under the normally writable host /tmp ensures
    // the probe can distinguish mount EROFS from ordinary ownership denial.
    let marker = Path::new("/tmp").join(format!(
        "chaos-warden-write-refusal-{}",
        f._temp.path().file_name().unwrap().to_str().unwrap()
    ));
    assert!(!marker.exists());
    let result = Command::new(&f.helper)
        .args(["--warden-launch", "--user", &user, "--"])
        .arg(harness)
        .arg(&f.helper)
        .arg("--check")
        .arg(&f.probe)
        .arg(&marker)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("warden-check: jailed non-root"),
        "{result:?}"
    );
    assert!(!marker.exists(), "readonly target created a host file");
}

#[test]
#[ignore = "requires root, loaded ABI-1 module and freshly built main harness/helper"]
fn live_main_sandbox_cli_with_production_launcher() {
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "run this explicit test as root"
    );
    let f = Fixture::new();
    let main = PathBuf::from(
        std::env::var_os("CHAOS_FREEBSD_TEST_MAIN").expect("set CHAOS_FREEBSD_TEST_MAIN"),
    );
    assert!(main.is_absolute() && main.is_file());
    let user = std::env::var("CHAOS_FREEBSD_TEST_USER")
        .expect("set CHAOS_FREEBSD_TEST_USER to an existing non-root account");
    fs::set_permissions(f._temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&f.probe, fs::Permissions::from_mode(0o755)).unwrap();
    let marker = Path::new("/tmp").join(format!(
        "chaos-main-write-refusal-{}",
        f._temp.path().file_name().unwrap().to_str().unwrap()
    ));
    assert!(!marker.exists());
    let run = |args: &[&std::ffi::OsStr]| {
        Command::new(&f.helper)
            .args(["--warden-launch", "--user", &user, "--"])
            .arg(&main)
            .args(["sandbox", "--"])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let confined = run(&[
        f.probe.as_os_str(),
        std::ffi::OsStr::new("warden-check"),
        marker.as_os_str(),
    ]);
    assert!(confined.status.success(), "{confined:?}");
    assert!(
        String::from_utf8_lossy(&confined.stdout).contains("warden-check: jailed non-root"),
        "{confined:?}"
    );
    assert!(!marker.exists(), "main CLI created a forbidden host file");
    for (script, code) in [
        ("printf 'main-sandbox-probe\\n'; exit 37", 37),
        ("kill -TERM $$", 128 + libc::SIGTERM),
    ] {
        let result = run(&[
            std::ffi::OsStr::new("/bin/sh"),
            std::ffi::OsStr::new("-c"),
            std::ffi::OsStr::new(script),
        ]);
        assert_eq!(result.status.code(), Some(code), "{result:?}");
        if code == 37 {
            assert_eq!(result.stdout, b"main-sandbox-probe\n", "{result:?}");
        }
    }
}

#[test]
#[ignore = "requires an explicitly loaded experimental kernel module"]
fn live_parser_rejects_malformed_labels_without_asserting() {
    let f = Fixture::new();
    for label in [
        "gulag/restrict",
        "gulag/restrict!v=0",
        "gulag/restrict!v=2",
        "gulag/restrict!v=1!",
        "gulag/restrict!v=1!w=",
        "gulag/restrict!v=1!w=relative",
        "gulag/restrict!v=1!w=/tmp/../",
        "gulag/restrict!v=1!w=/tmp//bad",
        "gulag/restrict!v=1!w=/tmp%00/escape",
        "gulag/restrict!v=1!w=/tmp%zz",
        "gulag/restrict!v=1!bogus",
        "gulag/restrict!v=1!r=/tmp",
        "gulag/restrict!v=1!net=deny!net=deny",
    ] {
        let output = Command::new(&f.probe)
            .args(["setlabel", label])
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted malformed label {label}");
    }
    let output = Command::new(&f.probe)
        .args(["setlabel", "gulag/restrict!v=1!net=deny"])
        .output()
        .unwrap();
    assert!(output.status.success(), "ABI-1 parser failed: {output:?}");
}
