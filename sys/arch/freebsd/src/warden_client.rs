//! Early bootstrap capture and explicit inheritance for sandbox helpers only.
//! Capture must run before the harness creates any threads or subprocesses.

use crate::warden_channel::Channel;
use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::OnceLock;

pub const CONTROL_ENV: &str = "CHAOS_WARDEN_CONTROL_FD";
static CONTROL: OnceLock<Channel> = OnceLock::new();

/// # Safety
/// Must run at process startup before any concurrent environment access,
/// fork/exec, or FD-table mutation. An inherited control FD is uniquely owned.
pub unsafe fn initialize() -> io::Result<()> {
    let Some(value) = std::env::var_os(CONTROL_ENV) else {
        return Ok(());
    };
    if CONTROL.get().is_some() {
        return Err(io::Error::other("Warden already initialized"));
    }
    // This is a launcher capability, not a value to propagate via ambient env.
    unsafe {
        std::env::remove_var(CONTROL_ENV);
    }
    let channel = take(value)?;
    CONTROL
        .set(channel)
        .map_err(|_| io::Error::other("Warden already initialized"))
}

fn take(value: std::ffi::OsString) -> io::Result<Channel> {
    let fd: RawFd = value
        .to_str()
        .and_then(|value| value.parse().ok())
        .filter(|fd| *fd > 2)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid Warden bootstrap descriptor",
            )
        })?;
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // Set CLOEXEC before symbol resolution, credential checks or other work.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let channel = Channel::from_owned(unsafe { OwnedFd::from_raw_fd(fd) })?;
    channel.require_root_peer()?;
    Ok(channel)
}

pub(crate) fn prepared_environment() -> Option<String> {
    CONTROL
        .get()
        .map(|channel| channel.as_fd().as_raw_fd().to_string())
}

/// Called by actual spawn sites, not by arbitrary target requests. Returns
/// only a process-owned initialized capability, never an FD chosen by env text.
pub fn inherited_fd(arg0: Option<&str>, value: Option<&str>) -> io::Result<Option<RawFd>> {
    if arg0 != Some("alcatraz") || value.is_none() {
        return Ok(None);
    }
    let channel = CONTROL.get().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "no initialized Warden bootstrap",
        )
    })?;
    let fd = channel.as_fd().as_raw_fd();
    if value != Some(fd.to_string().as_str()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Warden descriptor substitution refused",
        ));
    }
    Ok(Some(fd))
}

/// Wire the authenticated helper capability into a standard Command. Only the
/// forked child clears CLOEXEC; the harness's owned descriptor stays protected.
/// Pipe/PTY backends that accept an explicit inherited-FD list use inherited_fd.
pub fn inherit_for_helper(
    command: &mut Command,
    arg0: Option<&str>,
    value: Option<&str>,
) -> io::Result<()> {
    if let Some(fd) = inherited_fd(arg0, value)? {
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    Ok(())
}

/// # Safety
/// Single-threaded helper only; the environment names a uniquely inherited FD.
pub(crate) unsafe fn take_helper_channel() -> io::Result<Option<Channel>> {
    let Some(value) = std::env::var_os(CONTROL_ENV) else {
        return Ok(None);
    };
    unsafe {
        std::env::remove_var(CONTROL_ENV);
    }
    take(value).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_cannot_select_an_unowned_descriptor_for_a_target() {
        assert_eq!(inherited_fd(Some("/bin/sh"), Some("3")).unwrap(), None);
        assert_eq!(inherited_fd(None, Some("3")).unwrap(), None);
        assert_eq!(inherited_fd(Some("alcatraz"), None).unwrap(), None);
        // No default test installs a process-global root capability.
        assert!(inherited_fd(Some("alcatraz"), Some("3")).is_err());
        let mut command = Command::new("/bin/sh");
        assert!(inherit_for_helper(&mut command, None, Some("3")).is_ok());
        assert!(inherit_for_helper(&mut command, Some("alcatraz"), Some("3")).is_err());
        assert!(take("0".into()).is_err());
        assert!(take("-1".into()).is_err());
        assert!(take("not-an-fd".into()).is_err());
    }
}
