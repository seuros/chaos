//! Warden jail-ownership regressions.
use super::*;
use std::os::unix::fs::PermissionsExt;

fn integer(options: &JailOptions, name: &CStr) -> i32 {
    let bytes = &options
        .values
        .iter()
        .find(|(key, _)| *key == name)
        .unwrap()
        .1;
    i32::from_ne_bytes(bytes.as_slice().try_into().unwrap())
}

#[test]
fn creation_is_new_owner_only_never_attach_or_update() {
    assert_eq!(CREATE_FLAGS, 0x81);
    assert_eq!(CREATE_FLAGS & libc::JAIL_ATTACH, 0);
    assert_eq!(CREATE_FLAGS & libc::JAIL_UPDATE, 0);
}

#[test]
fn no_network_parameters_and_privileges_are_explicit() {
    let options = JailOptions::new(
        Path::new("/private/jail"),
        "chaos-test",
        SocketPolicy::Restricted,
    )
    .unwrap();
    assert_eq!(integer(&options, c"ip4"), libc::JAIL_SYS_DISABLE);
    assert_eq!(integer(&options, c"ip6"), libc::JAIL_SYS_DISABLE);
    assert_eq!(integer(&options, c"children.max"), 0);
    assert_eq!(integer(&options, c"enforce_statfs"), 2);
    for flag in [
        c"persist",
        c"allow.nomount",
        c"allow.nosuser",
        c"allow.noraw_sockets",
    ] {
        assert!(
            options
                .values
                .iter()
                .any(|(name, value)| *name == flag && value.is_empty())
        );
    }
    assert!(options.returned_descriptor().is_err());
}

#[test]
fn enabled_network_uses_intentional_inheritance_not_disabled_addresses() {
    let options = JailOptions::new(
        Path::new("/private/jail"),
        "chaos-test",
        SocketPolicy::Enabled,
    )
    .unwrap();
    assert_eq!(integer(&options, c"ip4"), libc::JAIL_SYS_INHERIT);
    assert_eq!(integer(&options, c"ip6"), libc::JAIL_SYS_INHERIT);
}

#[test]
fn names_cannot_select_hierarchical_or_existing_unrelated_namespaces() {
    for name in [
        "host",
        "chaos-a.child",
        "chaos-/path",
        "chaos-\0bad",
        &"a".repeat(90),
    ] {
        assert!(
            JailOptions::new(Path::new("/private/jail"), name, SocketPolicy::Restricted).is_err()
        );
    }
}

#[test]
fn iovecs_hold_nul_names_and_correctly_sized_native_values() {
    let mut options = JailOptions::new(
        Path::new("/private/jail"),
        "chaos-test",
        SocketPolicy::Restricted,
    )
    .unwrap();
    let iov = options.iovecs();
    assert_eq!(iov.len(), options.values.len() * 2);
    for (pair, (name, value)) in iov.as_chunks::<2>().0.iter().zip(&options.values) {
        assert_eq!(pair[0].iov_base.cast_const(), name.as_ptr().cast());
        assert_eq!(pair[0].iov_len, name.to_bytes_with_nul().len());
        assert_eq!(pair[1].iov_len, value.len());
        if value.is_empty() {
            assert!(pair[1].iov_base.is_null());
        } else {
            assert_eq!(pair[1].iov_base.cast_const(), value.as_ptr().cast());
        }
    }
}

#[test]
fn plain_or_writable_root_is_not_a_verified_readonly_mount() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(ReadOnlyRootMount::open(temp.path()).is_err());
    assert!(ReadOnlyRootMount::open(Path::new("/")).is_err());
    assert!(ReadOnlyRootMount::open(Path::new("relative")).is_err());
}

#[test]
fn descriptor_is_cloexec_and_closed_before_root_is_returned() {
    let temp = tempfile::tempdir().unwrap();
    let mut pipe = [-1; 2];
    // This tests FD ownership only, with a pipe standing in for the kernel's
    // returned descriptor. It creates no jail and does not test prison removal.
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let owner = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
    let _writer = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    protect_owner_descriptor(&owner).unwrap();
    assert_ne!(
        unsafe { libc::fcntl(pipe[0], libc::F_GETFD) } & libc::FD_CLOEXEC,
        0
    );
    assert_ne!(
        unsafe { libc::fcntl(pipe[0], libc::F_GETFD) } & FD_CLOFORK,
        0
    );
    let jail = OwnedJail {
        owner,
        root: ReadOnlyRootMount {
            path: temp.path().to_owned(),
            directory: File::open(temp.path()).unwrap(),
        },
        diagnostic_jid: 123,
        name: "chaos-test".to_owned(),
    };
    let root = jail.begin_remove();
    assert_eq!(unsafe { libc::fcntl(pipe[0], libc::F_GETFD) }, -1);
    assert!(root.directory.metadata().is_ok());
}

#[test]
fn owning_descriptor_is_not_inherited_by_forked_commands() {
    let mut pipe = [-1; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let owner = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
    let _writer = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
    protect_owner_descriptor(&owner).unwrap();
    // Child uses only async-signal-safe C calls and _exit, never Rust
    // allocation, destructors, logging, or the multithreaded test runtime.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        let missing = unsafe { libc::fcntl(pipe[0], libc::F_GETFD) } == -1;
        unsafe { libc::_exit(if missing { 0 } else { 1 }) };
    }
    let mut status = 0;
    loop {
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            break;
        }
        assert_eq!(result, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EINTR));
    }
    assert!(libc::WIFEXITED(status));
    assert_eq!(libc::WEXITSTATUS(status), 0);
    assert!(unsafe { libc::fcntl(pipe[0], libc::F_GETFD) } >= 0);
}
