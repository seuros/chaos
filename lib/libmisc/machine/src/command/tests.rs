use super::*;

#[test]
fn captures_both_pipes_exit_status_literal_arguments_and_c_locale() {
    let output = run(
        "/bin/sh",
        &[
            OsStr::new("-c"),
            OsStr::new("printf '%s\\n%s' \"$LC_ALL\" \"$1\"; printf stderr >&2; exit 7"),
            OsStr::new("probe"),
            OsStr::new("/Volumes/a space; $(not-executed)"),
        ],
        Duration::from_secs(2),
    )
    .expect("capture failed command");
    assert!(!output.success);
    assert_eq!(output.stdout, b"C\n/Volumes/a space; $(not-executed)");
    assert_eq!(output.stderr, b"stderr");
}

#[test]
fn deadlines_and_both_output_limits_kill_and_reap() {
    for (body, timeout, expected) in [
        (":", Duration::from_millis(150), Error::TimedOut),
        (
            "printf '%4096s' x",
            Duration::from_secs(2),
            Error::Unavailable("probe output exceeded limit".into()),
        ),
        (
            "printf '%4096s' x >&2",
            Duration::from_secs(2),
            Error::Unavailable("probe output exceeded limit".into()),
        ),
    ] {
        let directory = tempfile::tempdir().expect("pid directory");
        let pid_path = directory.path().join("pid");
        let script = format!("printf '%s' \"$$\" > \"$1\"; while :; do {body}; done");
        let started = Instant::now();
        let result = run(
            "/bin/sh",
            &[
                OsStr::new("-c"),
                OsStr::new(&script),
                OsStr::new("probe"),
                pid_path.as_os_str(),
            ],
            timeout,
        );
        assert_eq!(result.expect_err(body), expected);
        assert!(started.elapsed() < Duration::from_secs(3));
        let pid: libc::pid_t = std::fs::read_to_string(pid_path)
            .expect("child published pid")
            .parse()
            .expect("child pid");
        let mut status = 0;
        // SAFETY: status is writable; waitpid only queries this test's child PID.
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}

#[test]
fn spawn_failures_preserve_permission_denied() {
    let Err(Error::Unavailable(_)) = run("/nonexistent/chaos-probe", &[], Duration::from_secs(1))
    else {
        panic!("missing probe must be unavailable");
    };
    assert_eq!(
        Error::from(io::Error::from(io::ErrorKind::PermissionDenied)),
        Error::PermissionDenied
    );
}
