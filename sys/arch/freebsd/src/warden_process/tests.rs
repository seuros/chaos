use super::*;
use crate::warden_jail::ReadOnlyRootMount;
use crate::warden_mount::ScopedMounts;
use chaos_ipc::protocol::SocketPolicy;
use std::io::Read;
use std::process::Stdio;
use std::time::{Duration, Instant};

#[test]
fn command_identities_are_bounded_and_never_root() {
    assert!(CommandIdentity::new(0, 0, vec![]).is_err());
    assert!(CommandIdentity::new(u32::MAX, 1, vec![]).is_err());
    assert!(CommandIdentity::new(1001, 1, vec![u32::MAX]).is_err());
    assert!(CommandIdentity::new(1001, 1, vec![1; 1025]).is_err());
    assert!(CommandIdentity::new(1001, 1001, vec![1001]).is_ok());
}

#[test]
fn descriptor_snapshot_handles_packed_records_and_rejects_truncation() {
    fn entry(size: usize, fd: i32) -> Vec<u8> {
        let mut bytes = vec![0; size];
        bytes[..4].copy_from_slice(&(size as i32).to_ne_bytes());
        let offset = std::mem::offset_of!(libc::kinfo_file, kf_fd);
        bytes[offset..offset + 4].copy_from_slice(&fd.to_ne_bytes());
        bytes
    }
    let small = std::mem::offset_of!(libc::kinfo_file, kf_fd) + 4;
    let bytes: Vec<_> = [-1, 0, 1, 2, 3, 7, 19, 19]
        .into_iter()
        .flat_map(|fd| entry(small + (fd.max(0) as usize % 3), fd))
        .collect();
    assert_eq!(parse_descriptors(&bytes, 7).unwrap(), vec![3, 19]);
    for invalid in [&bytes[..bytes.len() - 1], &[0, 0, 0, 0], &[1], &[255; 4]] {
        assert!(parse_descriptors(invalid, 7).is_err());
    }
}

#[test]
fn native_descriptor_snapshot_covers_open_directory_without_closing_parent_copy() {
    use std::os::fd::AsRawFd;
    let directory = std::fs::File::open("/").unwrap();
    let inherited = inherited_descriptors(-1).unwrap();
    assert!(inherited.contains(&directory.as_raw_fd()));
    assert!(directory.metadata().unwrap().is_dir());
}

#[test]
#[ignore = "explicit root-only native jail/child lifecycle test; no custom MAC module"]
fn native_readonly_jail_exec_drops_identity_and_removes_background_children() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    // This validates native primitives only, not a full host view/policy or
    // MAC network enforcement. No child mount mirrors or /dev are needed
    // for these bounded base-system commands.
    let mut mounts = ScopedMounts::create(Path::new("/tmp")).unwrap();
    let state = mounts.state_path().to_owned();
    mounts.nullfs(Path::new("/"), Path::new(""), true).unwrap();
    let root = ReadOnlyRootMount::open(&mounts.view_path()).unwrap();
    let name = format!("chaos-native-exec-{}", std::process::id());
    let jail = OwnedJail::create(root, &name, SocketPolicy::Restricted).unwrap();
    let identity = CommandIdentity::new(65534, 65534, vec![65534]).unwrap();
    let mut command = Command::new("/bin/sh");
    command.env_clear()
        .args(["-c", "/usr/bin/id -u; /sbin/sysctl -n security.jail.jailed; /bin/sleep 60 & echo background; exit 0"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = spawn_inner(&jail, command, &identity, Path::new("/"), None).unwrap();
    assert!(child.wait().unwrap().success());
    // The background child still holds the output pipes. Removing the owner
    // must kill the whole prison before we wait for EOF / unmount the view.
    drop(jail.begin_remove());
    let mut stdout = child.stdout.take().unwrap();
    let mut output = String::new();
    stdout.read_to_string(&mut output).unwrap();
    assert_eq!(output, "65534\n1\nbackground\n");
    drop(stdout);
    drop(child);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match mounts.cleanup() {
            Ok(()) => break,
            Err(error)
                if error.raw_os_error() == Some(libc::EBUSY) && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => panic!("cleanup: {error}"),
        }
    }
    assert!(!state.exists());
}

#[test]
#[ignore = "explicit root-only native failed-exec rollback test; no custom MAC module"]
fn native_failed_exec_preserves_error_pipe_and_jail_removal_authority() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let mut mounts = ScopedMounts::create(Path::new("/tmp")).unwrap();
    let state = mounts.state_path().to_owned();
    mounts.nullfs(Path::new("/"), Path::new(""), true).unwrap();
    let root = ReadOnlyRootMount::open(&mounts.view_path()).unwrap();
    let jail = OwnedJail::create(
        root,
        &format!("chaos-native-fail-{}", std::process::id()),
        SocketPolicy::Restricted,
    )
    .unwrap();
    let identity = CommandIdentity::new(65534, 65534, vec![65534]).unwrap();
    let error = spawn_inner(
        &jail,
        Command::new("/chaos-intentionally-missing-executable"),
        &identity,
        Path::new("/"),
        None,
    )
    .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
    // Failed child setup must not consume Warden's original owner.
    assert!(jail.attachment().is_ok());
    drop(jail.begin_remove());
    mounts.cleanup().unwrap();
    assert!(!state.exists());
}

#[test]
#[ignore = "explicit root-only full read-only view test; temporary jail/mounts, no custom MAC module"]
fn native_host_view_mirrors_mounts_provides_minimal_devices_and_executes() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir().unwrap();
    std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let marker = fixture.path().join("marker");
    std::fs::write(&marker, "host-mount-visible\n").unwrap();
    let plan = crate::warden_plan::FilesystemPlan::ReadOnlyHostView { overlays: vec![] };
    let (mut mounts, root) = crate::warden_view::provision(&plan, Path::new("/tmp")).unwrap();
    let state = mounts.state_path().to_owned();
    let relative = marker.strip_prefix("/").unwrap();
    assert_eq!(
        std::fs::read(mounts.view_path().join(relative)).unwrap(),
        std::fs::read(&marker).unwrap()
    );
    assert_eq!(
        std::fs::write(mounts.view_path().join(relative), b"not permitted")
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EROFS)
    );
    let jail = OwnedJail::create(
        root,
        &format!("chaos-native-view-{}", std::process::id()),
        SocketPolicy::Restricted,
    )
    .unwrap();
    let identity = CommandIdentity::new(65534, 65534, vec![65534]).unwrap();
    let mut command = Command::new("/bin/sh");
    command.env_clear().args(["-c",
        "test ! -e /dev/zfs && test ! -e /dev/fd && /bin/cat \"$1\" > /dev/null && /bin/cat \"$1\"",
        "view-test"]).arg(&marker)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = spawn_inner(&jail, command, &identity, Path::new("/"), None).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, b"host-mount-visible\n");
    drop(jail.begin_remove());
    mounts.cleanup().unwrap();
    assert!(!state.exists());
}
