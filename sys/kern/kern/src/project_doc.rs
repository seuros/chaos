//! Project-level documentation discovery.
//!
//! Project-level documentation is primarily stored in files named `AGENTS.md`.
//! Additional fallback filenames can be configured via `project_doc_fallback_filenames`.
//! We include the concatenation of all files found along the path from the
//! project root to the current working directory as follows:
//!
//! 1.  Determine the project root by walking upwards from the current working
//!     directory until a configured `project_root_markers` entry is found.
//!     When `project_root_markers` is unset, the default marker list is used
//!     (`.git`). If no marker is found, only the current working directory is
//!     considered. An empty marker list disables parent traversal.
//! 2.  Collect every `AGENTS.md` found from the project root down to the
//!     current working directory (inclusive) and concatenate their contents in
//!     that order.
//! 3.  We do **not** walk past the project root.

use crate::config::Config;
use crate::config_loader::ConfigLayerStackOrdering;
use crate::config_loader::default_project_root_markers;
use crate::config_loader::merge_toml_values;
use crate::config_loader::project_root_markers_from_config;
use crate::features::Feature;
use chaos_ipc::api::ConfigLayerSource;
use dunce::canonicalize as normalize_path;
use std::path::PathBuf;
use tokio::io::AsyncReadExt;
use toml::Value as TomlValue;
use tracing::error;

pub(crate) const HIERARCHICAL_AGENTS_MESSAGE: &str =
    include_str!("../hierarchical_agents_message.md");

/// Default filename scanned for project-level docs.
pub const DEFAULT_PROJECT_DOC_FILENAME: &str = "AGENTS.md";
/// Preferred local override for project-level docs.
pub const LOCAL_PROJECT_DOC_FILENAME: &str = "AGENTS.override.md";

/// When both `Config::instructions` and the project doc are present, they will
/// be concatenated with the following separator.
const PROJECT_DOC_SEPARATOR: &str = "\n\n--- project-doc ---\n\n";

/// Combines `Config::instructions` and `AGENTS.md` (if present) into a single
/// string of instructions.
pub(crate) async fn get_user_instructions(config: &Config) -> Option<String> {
    let project_docs = read_project_docs(config).await;

    let mut output = String::new();

    if let Some(instructions) = config.user_instructions.clone() {
        output.push_str(&instructions);
    }

    match project_docs {
        Ok(Some(docs)) => {
            if !output.is_empty() {
                output.push_str(PROJECT_DOC_SEPARATOR);
            }
            output.push_str(&docs);
        }
        Ok(None) => {}
        Err(e) => {
            error!("error trying to find project doc: {e:#}");
        }
    };

    if config.features.enabled(Feature::ChildAgentsMd) {
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(HIERARCHICAL_AGENTS_MESSAGE);
    }

    if !output.is_empty() {
        Some(output)
    } else {
        None
    }
}

/// Attempt to locate and load the project documentation.
///
/// On success returns `Ok(Some(contents))` where `contents` is the
/// concatenation of all discovered docs. If no documentation file is found the
/// function returns `Ok(None)`. Unexpected I/O failures bubble up as `Err` so
/// callers can decide how to handle them.
pub async fn read_project_docs(config: &Config) -> std::io::Result<Option<String>> {
    let max_total = config.project_doc_max_bytes;

    if max_total == 0 {
        return Ok(None);
    }

    let paths = discover_project_doc_paths(config)?;
    if paths.is_empty() {
        return Ok(None);
    }

    let mut remaining: u64 = max_total as u64;
    let mut parts: Vec<String> = Vec::new();

    for p in paths {
        if remaining == 0 {
            break;
        }

        let file = match tokio::fs::File::open(&p).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };

        let size = file.metadata().await?.len();
        let mut reader = tokio::io::BufReader::new(file).take(remaining);
        let mut data: Vec<u8> = Vec::new();
        reader.read_to_end(&mut data).await?;

        if size > remaining {
            tracing::warn!(
                "Project doc `{}` exceeds remaining budget ({} bytes) - truncating.",
                p.display(),
                remaining,
            );
        }

        let text = String::from_utf8_lossy(&data).to_string();
        if !text.trim().is_empty() {
            parts.push(text);
            remaining = remaining.saturating_sub(data.len() as u64);
        }
    }

    if parts.is_empty() {
        Ok(None)
    } else {
        Ok(Some(parts.join("\n\n")))
    }
}

/// Discover the list of AGENTS.md files using the same search rules as
/// `read_project_docs`, but return the file paths instead of concatenated
/// contents. The list is ordered from project root to the current working
/// directory (inclusive). Symlinks are allowed. When `project_doc_max_bytes`
/// is zero, returns an empty list.
pub fn discover_project_doc_paths(config: &Config) -> std::io::Result<Vec<PathBuf>> {
    let mut dir = config.cwd.clone();
    if let Ok(canon) = normalize_path(&dir) {
        dir = canon;
    }

    let mut merged = TomlValue::Table(toml::map::Map::new());
    for layer in config.config_layer_stack.get_layers(
        ConfigLayerStackOrdering::LowestPrecedenceFirst,
        /*include_disabled*/ false,
    ) {
        if matches!(layer.name, ConfigLayerSource::Project { .. }) {
            continue;
        }
        merge_toml_values(&mut merged, &layer.config);
    }
    let project_root_markers = match project_root_markers_from_config(&merged) {
        Ok(Some(markers)) => markers,
        Ok(None) => default_project_root_markers(),
        Err(err) => {
            tracing::warn!("invalid project_root_markers: {err}");
            default_project_root_markers()
        }
    };
    let mut project_root = None;
    if !project_root_markers.is_empty() {
        for ancestor in dir.ancestors() {
            for marker in &project_root_markers {
                let marker_path = ancestor.join(marker);
                let marker_exists = match std::fs::metadata(&marker_path) {
                    Ok(_) => true,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                    Err(e) => return Err(e),
                };
                if marker_exists {
                    project_root = Some(ancestor.to_path_buf());
                    break;
                }
            }
            if project_root.is_some() {
                break;
            }
        }
    }

    let search_dirs: Vec<PathBuf> = if let Some(root) = project_root {
        let mut dirs = Vec::new();
        let mut cursor = dir.as_path();
        loop {
            dirs.push(cursor.to_path_buf());
            if cursor == root {
                break;
            }
            let Some(parent) = cursor.parent() else {
                break;
            };
            cursor = parent;
        }
        dirs.reverse();
        dirs
    } else {
        vec![dir]
    };

    let mut found: Vec<PathBuf> = Vec::new();
    let candidate_filenames = candidate_filenames(config);
    for d in search_dirs {
        for name in &candidate_filenames {
            let candidate = d.join(name);
            match std::fs::symlink_metadata(&candidate) {
                Ok(md) => {
                    let ft = md.file_type();
                    // Allow regular files and symlinks; opening will later fail for dangling links.
                    if ft.is_file() || ft.is_symlink() {
                        found.push(candidate);
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            }
        }
    }

    Ok(found)
}

fn candidate_filenames<'a>(config: &'a Config) -> Vec<&'a str> {
    let mut names: Vec<&'a str> =
        Vec::with_capacity(2 + config.project_doc_fallback_filenames.len());
    names.push(LOCAL_PROJECT_DOC_FILENAME);
    names.push(DEFAULT_PROJECT_DOC_FILENAME);
    for candidate in &config.project_doc_fallback_filenames {
        let candidate = candidate.as_str();
        if candidate.is_empty() {
            continue;
        }
        if !names.contains(&candidate) {
            names.push(candidate);
        }
    }
    names
}

#[cfg(test)]
#[path = "project_doc_tests.rs"]
mod tests;
