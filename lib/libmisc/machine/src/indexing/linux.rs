use super::IndexerProcess;
use crate::command::{Error, Output};
use std::path::PathBuf;

// Filesystem/content indexer basenames.
const INDEXERS: &[&str] = &[
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
