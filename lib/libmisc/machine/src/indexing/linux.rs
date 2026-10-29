use super::IndexerProcess;
use super::process::INDEXERS;
use crate::command::{Error, Output};
use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub(super) fn inspect(mut snapshot: super::IndexingSnapshot) -> super::IndexingSnapshot {
    use super::process::observation;

    snapshot.processes = observation(super::process::query().and_then(|output| {
        processes(&output, |pid| {
            std::fs::read_link(format!("/proc/{pid}/exe"))
        })
    }));
    snapshot
}

fn processes(
    output: &Output,
    mut executable: impl FnMut(u32) -> std::io::Result<PathBuf>,
) -> Result<Vec<IndexerProcess>, Error> {
    // comm truncates at 15 bytes; procfs confirms the full basename.
    let candidates = super::process::processes(output, |name| {
        INDEXERS
            .iter()
            .any(|known| name == *known || name == &known[..known.len().min(15)])
    })?;
    let mut value = Vec::new();
    for mut process in candidates {
        let path = match executable(process.pid) {
            Ok(path) => path,
            // The candidate may have exited.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        // procfs marks unlinked executables.
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.strip_suffix(" (deleted)").unwrap_or(name));
        if let Some(name) = name.filter(|name| INDEXERS.contains(name)) {
            process.name = name.to_owned();
            value.push(process);
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests;
