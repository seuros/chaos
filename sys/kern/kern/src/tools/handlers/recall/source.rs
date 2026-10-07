#[cfg(unix)]
use std::ffi::CString;
use std::fs::File;
use std::io::{Error, Read};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::path::Component;
use std::path::Path;

use chaos_ipc::protocol::VfsPolicy;
use chaos_recall::{MemorySource, RecallError};
use serde_json::json;

const SOURCE_BYTES: u64 = 32_768;

pub(super) fn read(
    source: &MemorySource,
    policy: &VfsPolicy,
    cwd: &Path,
) -> Result<String, RecallError> {
    source.validate()?;
    let MemorySource::File { path, fragment } = source;
    let denied =
        || RecallError::InvalidInput("source is not readable under the current filesystem policy");
    if !chaos_parole::sandbox::can_read_path(policy, path, cwd) {
        return Err(denied());
    }
    let canonical = path.canonicalize().map_err(|_| denied())?;
    if !chaos_parole::sandbox::can_read_path(policy, &canonical, cwd) {
        return Err(denied());
    }
    let file = open(&canonical).map_err(|_| denied())?;
    let metadata = file.metadata().map_err(|_| denied())?;
    if !metadata.is_file() || metadata.len() > SOURCE_BYTES {
        return Err(RecallError::InvalidInput(
            "source must be a regular file of at most 32768 bytes",
        ));
    }
    let mut bytes = Vec::new();
    file.take(SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| denied())?;
    if bytes.len() as u64 > SOURCE_BYTES {
        return Err(RecallError::InvalidInput("source exceeds the byte budget"));
    }
    let content = String::from_utf8(bytes)
        .map_err(|_| RecallError::InvalidInput("source is not UTF-8 text"))?;
    let output = json!({"source": source, "resolved_path": canonical, "fragment": fragment, "content": content}).to_string();
    if output.len() > super::OUTPUT_BYTES {
        return Err(RecallError::InvalidInput(
            "source exceeds the output byte budget",
        ));
    }
    Ok(output)
}

#[cfg(unix)]
fn open(path: &Path) -> Result<File, Error> {
    let mut directory = File::open("/")?;
    let components = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect::<Vec<_>>();
    for (index, part) in components.iter().enumerate() {
        let name = CString::new(part.as_bytes())?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if index + 1 < components.len() {
                libc::O_DIRECTORY
            } else {
                0
            };
        // Each descriptor anchors traversal; no component can become a followed symlink.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        // openat returned a new descriptor, owned exclusively by this File.
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}

#[cfg(not(unix))]
fn open(_: &Path) -> Result<File, Error> {
    Err(Error::new(
        std::io::ErrorKind::Unsupported,
        "safe source opening is unavailable on this platform",
    ))
}

#[cfg(all(test, unix))]
mod tests;
