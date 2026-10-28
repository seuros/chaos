use super::SpotlightState;
use super::process::{processes, validated_text};
use crate::command::{Error, Output};

#[cfg(target_os = "macos")]
pub(super) fn inspect(mut snapshot: super::IndexingSnapshot) -> super::IndexingSnapshot {
    use super::process::observation;
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
    snapshot.processes =
        observation(super::process::query().and_then(|output| processes(&output, is_indexer)));
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

fn spotlight(output: &Output) -> Result<SpotlightState, Error> {
    let text = validated_text(output, "invalid mdutil output encoding")?;
    // Ignore state words in paths and diagnostics.
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
