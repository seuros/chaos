//! Small bounded subprocess runner: drain both pipes without reader threads.

use std::ffi::OsStr;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
const ERROR_LIMIT: usize = 16 * 1024;

#[derive(Debug)]
pub(crate) struct Output {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Error {
    Unavailable(String),
    PermissionDenied,
    TimedOut,
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::PermissionDenied {
            Self::PermissionDenied
        } else {
            Self::Unavailable(error.to_string())
        }
    }
}

pub(crate) fn run(program: &str, args: &[&OsStr], timeout: Duration) -> Result<Output, Error> {
    let mut child = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let result = (|| {
        let missing_pipe = || Error::Unavailable("probe output pipe unavailable".into());
        let mut stdout = child.stdout.take().ok_or_else(missing_pipe)?;
        let mut stderr = child.stderr.take().ok_or_else(missing_pipe)?;
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        let deadline = Instant::now() + timeout;
        let mut output = Output {
            success: false,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        let mut stdout_done = false;
        let mut stderr_done = false;
        loop {
            stdout_done |= drain(&mut stdout, &mut output.stdout, OUTPUT_LIMIT)?;
            stderr_done |= drain(&mut stderr, &mut output.stderr, ERROR_LIMIT)?;
            if let Some(status) = child.try_wait()? {
                output.success = status.success();
                if stdout_done && stderr_done {
                    return Ok(output);
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::TimedOut);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })();
    if result.is_err() {
        // Errors and timeouts must not leave a running child or zombie behind.
        let _ = child.kill();
        // Cleanup is best-effort; preserve the original probe failure.
        let _ = child.wait();
    }
    result
}

fn nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: the pipe owns a live fd; F_GETFL/F_SETFL take no pointer arguments.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is still live and F_SETFL receives only integer flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn drain(reader: &mut impl Read, bytes: &mut Vec<u8>, limit: usize) -> Result<bool, Error> {
    let mut buffer = [0; 8192];
    // Bound each turn as well as total output so an active pipe cannot starve
    // the other pipe or the deadline check.
    for _ in 0..16 {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) if bytes.len() + count <= limit => {
                bytes.extend_from_slice(&buffer[..count]);
            }
            Ok(_) => {
                return Err(Error::Unavailable("probe output exceeded limit".into()));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests;
