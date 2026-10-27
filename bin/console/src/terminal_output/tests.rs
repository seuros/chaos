use super::*;
use std::process::Command;

const CHILD_MODE: &str = "CHAOS_TEST_PANIC_OUTPUT";

#[test]
fn diagnostics_and_panic_hook_survive_terminal_errors() {
    if let Some(_mode) = std::env::var_os(CHILD_MODE) {
        install_panic_hook().expect("install production panic hook");
        #[cfg(target_os = "freebsd")]
        let _pty = (_mode == "revoked").then(revoke_stderr);
        write_stderr(format_args!("terminal cleanup diagnostic"));
        let panic = std::panic::catch_unwind(|| panic!("terminal panic regression"));
        assert!(
            panic.is_err(),
            "the hook must not swallow the original panic"
        );
        return;
    }

    let modes = [
        "healthy",
        #[cfg(target_os = "freebsd")]
        "revoked",
    ];
    for mode in modes {
        // Hooks and redirected stderr are process-global: isolate them from
        // parallel tests. A double panic aborts this child instead of the suite.
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "terminal_output::tests::diagnostics_and_panic_hook_survive_terminal_errors",
                "--nocapture",
            ])
            .env(CHILD_MODE, mode)
            .output()
            .expect("run panic output regression");
        assert!(
            output.status.success(),
            "{mode}: child failed: {:?}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "the isolated regression must actually run: {:?}",
            output.stdout
        );
        if mode == "healthy" {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("terminal cleanup diagnostic"), "{stderr}");
            assert!(stderr.contains("terminal panic regression"), "{stderr}");
        }
    }
}

#[cfg(target_os = "freebsd")]
fn revoke_stderr() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
    use std::os::fd::{AsRawFd, FromRawFd};

    // libc does not currently expose FreeBSD's revoke(2).
    unsafe extern "C" {
        fn revoke(path: *const libc::c_char) -> libc::c_int;
    }

    let mut master = -1;
    let mut slave = -1;
    // SAFETY: openpty initializes both descriptors; optional arguments are null.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0,
        "{}",
        io::Error::last_os_error()
    );
    // SAFETY: these are fresh descriptors owned only by this child.
    let master = unsafe { std::os::fd::OwnedFd::from_raw_fd(master) };
    let slave = unsafe { std::os::fd::OwnedFd::from_raw_fd(slave) };
    let mut name = [0 as libc::c_char; 128];
    // SAFETY: name is writable and its length is passed correctly.
    assert_eq!(
        unsafe { libc::ttyname_r(slave.as_raw_fd(), name.as_mut_ptr(), name.len()) },
        0
    );
    // Prevent a regressed panic hook from leaving another large core dump.
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: setrlimit reads the initialized limit, affecting only this child.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) }, 0);
    // SAFETY: redirect only this child's stderr to its freshly allocated PTY.
    assert_eq!(
        unsafe { libc::dup2(slave.as_raw_fd(), libc::STDERR_FILENO) },
        2
    );
    // SAFETY: ttyname_r returned a terminated path to this child's own PTY.
    assert_eq!(unsafe { revoke(name.as_ptr()) }, 0);
    assert_eq!(
        io::stderr()
            .write_all(b"revoked terminal probe")
            .expect_err("stderr must actually be revoked")
            .raw_os_error(),
        Some(libc::ENXIO)
    );
    (master, slave)
}
