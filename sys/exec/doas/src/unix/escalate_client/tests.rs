use super::*;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

#[test]
fn wrapper_env_filter_includes_retired_bash_variable() {
    assert!(is_wrapper_env_var(ESCALATE_SOCKET_ENV_VAR));
    assert!(is_wrapper_env_var(EXEC_WRAPPER_ENV_VAR));
    assert!(is_wrapper_env_var("BASH_EXEC_WRAPPER"));
    assert!(!is_wrapper_env_var("PATH"));
}

#[test]
fn duplicate_fd_for_transfer_does_not_close_original() {
    let (left, _right) = UnixStream::pair().expect("socket pair");
    let original_fd = left.as_raw_fd();

    let duplicate = duplicate_fd_for_transfer(&left, "test fd").expect("duplicate fd");
    assert_ne!(duplicate.as_raw_fd(), original_fd);

    drop(duplicate);

    assert_ne!(unsafe { libc::fcntl(original_fd, libc::F_GETFD) }, -1);
}
