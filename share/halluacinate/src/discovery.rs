//! Project script discovery.
//!
//! User scripts come from the runtime database (see `SessionInfo::user_scripts`).
//! Only the project layer is still file-based: `<cwd>/.chaos/scripts/*.lua`,
//! loaded after user scripts so it can override their registrations.

use std::path::Path;
use std::path::PathBuf;

/// Known script extensions and their engine backends.
const SCRIPT_EXTENSIONS: &[&str] = &["lua", "wasm"];

/// Project-layer scripts under `<cwd>/.chaos/scripts`, sorted by file name.
pub fn discover_project_scripts(cwd: &Path) -> Vec<PathBuf> {
    let mut scripts = Vec::new();
    collect_script_files(&cwd.join(".chaos").join("scripts"), &mut scripts);
    scripts
}

/// Collect all script files from a directory, sorted by name.
fn collect_script_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| SCRIPT_EXTENSIONS.contains(&ext))
        })
        .filter(|p| p.is_file())
        .collect();

    files.sort();
    out.extend(files);
}

#[cfg(test)]
mod tests;
