//! Shared validation of bounded `ps` observations.

use super::{IndexerProcess, IndexingObservation};
use crate::command::{Error, Output};

// Portable content-indexer and updatedb executable basenames.
#[cfg(any(target_os = "linux", target_os = "freebsd", test))]
pub(super) const INDEXERS: &[&str] = &[
    "tracker-store",
    "tracker-miner-fs",
    "tracker-miner-fs-3",
    "tracker-extract",
    "tracker-extract-3",
    "tracker-writeback",
    "tracker-writeback-3",
    "localsearch-3",
    "localsearch-extractor-3",
    "localsearch-writeback-3",
    "baloo_file",
    "baloo_file_extractor",
    "updatedb",
    "updatedb.mlocate",
    "updatedb.plocate",
];

pub(super) fn query() -> Result<Output, Error> {
    use std::ffi::OsStr;
    use std::time::Duration;

    crate::command::run(
        "/bin/ps",
        &[
            OsStr::new("-ww"),
            OsStr::new("-A"),
            OsStr::new("-o"),
            // FreeBSD treats everything after the first '=' as a header.
            OsStr::new("pid="),
            OsStr::new("-o"),
            OsStr::new("pcpu="),
            OsStr::new("-o"),
            OsStr::new("comm="),
        ],
        Duration::from_secs(1),
    )
}

pub(super) fn observation<T>(result: Result<T, Error>) -> IndexingObservation<T> {
    match result {
        Ok(value) => IndexingObservation::Available { value },
        Err(Error::Unavailable(reason)) => IndexingObservation::Unavailable { reason },
        Err(Error::PermissionDenied) => IndexingObservation::PermissionDenied,
        Err(Error::TimedOut) => IndexingObservation::TimedOut,
    }
}

pub(super) fn validated_text<'a>(output: &'a Output, invalid: &str) -> Result<&'a str, Error> {
    // Diagnose failures before parsing measurements.
    let diagnostic = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();
    if [
        "permission denied",
        "operation not permitted",
        "not authorized",
    ]
    .iter()
    .any(|message| diagnostic.contains(message))
    {
        Err(Error::PermissionDenied)
    } else if !output.success {
        Err(Error::Unavailable("probe exited unsuccessfully".into()))
    } else {
        std::str::from_utf8(&output.stdout).map_err(|_| Error::Unavailable(invalid.into()))
    }
}

pub(super) fn processes(
    output: &Output,
    matches: impl Fn(&str) -> bool,
) -> Result<Vec<IndexerProcess>, Error> {
    let invalid = || Error::Unavailable("unrecognized or incomplete ps output".into());
    let text = validated_text(output, "unrecognized or incomplete ps output")?;
    let mut value = Vec::new();
    let mut rows = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let process = process_row(line).ok_or_else(invalid)?;
        rows += 1;
        if matches(&process.name) {
            value.push(process);
        }
    }
    if rows == 0 {
        return Err(invalid());
    }
    value.sort_by_key(|process| process.pid);
    Ok(value)
}

fn process_row(line: &str) -> Option<IndexerProcess> {
    let line = line.trim_start();
    let (pid, rest) = line.split_once(char::is_whitespace)?;
    let (cpu, executable) = rest.trim_start().split_once(char::is_whitespace)?;
    let pid: u32 = pid.parse().ok()?;
    let cpu_percent: f64 = cpu.parse().ok()?;
    if !cpu_percent.is_finite() || cpu_percent < 0.0 {
        return None;
    }
    let name = executable.trim().rsplit('/').next()?;
    if name.is_empty() {
        return None;
    }
    Some(IndexerProcess {
        pid,
        name: name.to_owned(),
        cpu_percent,
    })
}

#[cfg(test)]
mod tests;
