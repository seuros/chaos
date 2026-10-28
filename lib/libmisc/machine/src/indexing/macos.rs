use super::{IndexerProcess, IndexingObservation, SpotlightState};
use crate::command::{Error, Output};

#[cfg(target_os = "macos")]
pub(super) fn inspect(mut snapshot: super::IndexingSnapshot) -> super::IndexingSnapshot {
    use crate::command::run;
    use std::ffi::OsStr;
    use std::time::Duration;

    let timeout = Duration::from_secs(1);
    snapshot.spotlight = observation(match workspace_volume(&snapshot.workspace) {
        Ok(volume) => {
            let result = run(
                "/usr/bin/mdutil",
                &[OsStr::new("-s"), volume.as_os_str()],
                timeout,
            );
            snapshot.volume = Some(volume);
            result.and_then(|output| spotlight(&output))
        }
        Err(error) => Err(error.into()),
    });
    snapshot.processes = observation(
        run(
            "/bin/ps",
            &[
                OsStr::new("-A"),
                OsStr::new("-o"),
                OsStr::new("pid=,pcpu=,comm="),
            ],
            timeout,
        )
        .and_then(|output| processes(&output)),
    );
    snapshot
}

#[cfg(target_os = "macos")]
fn workspace_volume(workspace: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use std::ffi::CString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    if !workspace.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace path must be absolute",
        ));
    }
    let workspace = workspace.canonicalize()?;
    let path = CString::new(workspace.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: path is NUL-terminated and stat points to writable statfs storage.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: statfs succeeded and initialized the structure.
    let stat = unsafe { stat.assume_init() };
    let bytes: Vec<u8> = stat
        .f_mntonname
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    let mount = std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes));
    if !mount.is_absolute() {
        return Err(std::io::Error::other("unrecognized workspace mount point"));
    }
    Ok(mount)
}

fn observation<T>(result: Result<T, Error>) -> IndexingObservation<T> {
    match result {
        Ok(value) => IndexingObservation::Available { value },
        Err(Error::Unavailable(reason)) => IndexingObservation::Unavailable { reason },
        Err(Error::PermissionDenied) => IndexingObservation::PermissionDenied,
        Err(Error::TimedOut) => IndexingObservation::TimedOut,
    }
}

fn validated_text<'a>(output: &'a Output, invalid: &str) -> Result<&'a str, Error> {
    // Check diagnostics before interpreting any output as a measurement.
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

fn spotlight(output: &Output) -> Result<SpotlightState, Error> {
    let text = validated_text(output, "invalid mdutil output encoding")?;
    // Do not substring-match "disabled" in an error or a volume's pathname.
    let mut states = text.lines().filter_map(|line| match line.trim() {
        "Indexing enabled." => Some(SpotlightState::Enabled),
        "Indexing disabled." | "Indexing and searching disabled." => Some(SpotlightState::Disabled),
        _ => None,
    });
    match (states.next(), states.next()) {
        (Some(value), None) => Ok(value),
        _ => Err(Error::Unavailable(
            "unrecognized or ambiguous mdutil indexing state".into(),
        )),
    }
}

fn processes(output: &Output) -> Result<Vec<IndexerProcess>, Error> {
    let invalid = || Error::Unavailable("unrecognized or incomplete ps output".into());
    let text = validated_text(output, "unrecognized or incomplete ps output")?;
    let mut value = Vec::new();
    let mut rows = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let process = process_row(line).ok_or_else(invalid)?;
        rows += 1;
        if is_indexer(&process.name) {
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

fn is_indexer(name: &str) -> bool {
    matches!(
        name,
        "mds"
            | "mds_stores"
            | "mdworker"
            | "mdworker_shared"
            | "mdworker32"
            | "mdbulkimport"
            | "mdflagwriter"
            | "mdwrite"
            | "corespotlightd"
            | "managedcorespotlightd"
            | "spotlightknowledged"
    )
}

#[cfg(test)]
mod tests;
