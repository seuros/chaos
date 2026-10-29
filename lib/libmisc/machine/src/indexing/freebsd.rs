//! FreeBSD accounting names select candidates; native paths confirm basenames.

use super::IndexerProcess;
use super::process::INDEXERS;
use crate::command::{Error, Output};
use std::path::PathBuf;
use std::time::Instant;

// locate.updatedb/mklocatedb are shell scripts. Observe their binary helpers,
// not arbitrary shell command lines or generic find/sort processes.
const NATIVE_INDEXERS: &[&str] = &["locate.code", "locate.bigram", "recollindex"];
const MAXCOMLEN: usize = 19;
const CANDIDATE_LIMIT: usize = 256;

#[cfg(target_os = "freebsd")]
pub(super) fn inspect(mut snapshot: super::IndexingSnapshot) -> super::IndexingSnapshot {
    use super::process::observation;
    use std::time::Duration;

    let deadline = Instant::now() + Duration::from_secs(1);
    snapshot.processes = observation(
        super::process::query().and_then(|output| processes(&output, executable, deadline)),
    );
    snapshot
}

fn processes(
    output: &Output,
    mut executable: impl FnMut(u32) -> std::io::Result<PathBuf>,
    deadline: Instant,
) -> Result<Vec<IndexerProcess>, Error> {
    let candidates = super::process::processes(output, |name| {
        INDEXERS
            .iter()
            .chain(NATIVE_INDEXERS)
            .any(|known| name == &known[..known.len().min(MAXCOMLEN)])
    })?;
    if candidates.len() > CANDIDATE_LIMIT {
        return Err(Error::Unavailable("too many indexer candidates".into()));
    }
    let mut value = Vec::new();
    for mut process in candidates {
        if Instant::now() >= deadline {
            return Err(Error::TimedOut);
        }
        let path = match executable(process.pid) {
            Ok(path) => path,
            // Exits between ps and sysctl are not permission failures.
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::ESRCH) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(name) = path.file_name().and_then(|name| name.to_str())
            && (INDEXERS.contains(&name) || NATIVE_INDEXERS.contains(&name))
        {
            process.name = name.to_owned();
            value.push(process);
        }
    }
    if Instant::now() >= deadline {
        return Err(Error::TimedOut);
    }
    Ok(value)
}

#[cfg(target_os = "freebsd")]
fn executable(pid: u32) -> std::io::Result<PathBuf> {
    use std::ffi::{CStr, OsString};
    use std::io;
    use std::os::unix::ffi::OsStringExt;

    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid process executable path",
        )
    };
    let pid = libc::pid_t::try_from(pid).map_err(|_| invalid())?;
    if pid <= 0 {
        return Err(invalid());
    }
    let mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PATHNAME,
        pid,
    ];
    let mut bytes = [0_u8; libc::PATH_MAX as usize];
    let mut size = bytes.len();
    // SAFETY: mib describes a read-only query. bytes/size are bounded writable
    // outputs; no new value is supplied. No procfs mount or privileges required.
    let result = unsafe {
        libc::sysctl(
            mib.as_ptr(),
            mib.len() as u32,
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let path =
        CStr::from_bytes_with_nul(bytes.get(..size).ok_or_else(invalid)?).map_err(|_| invalid())?;
    let path = PathBuf::from(OsString::from_vec(path.to_bytes().to_vec()));
    if !path.is_absolute() {
        return Err(invalid());
    }
    Ok(path)
}

#[cfg(test)]
mod tests;
