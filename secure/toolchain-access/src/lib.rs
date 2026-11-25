//! Discover repository tool output directories. Discovery never grants access.

mod cargo;
mod mbx;
mod mise;

#[cfg(test)]
mod tests;

use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Cargo,
    Mbx,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub tool: Tool,
    pub directory: PathBuf,
}

/// Startup observations, not permissions. Never re-read manifests during exec.
#[derive(Debug, Default)]
pub struct Snapshot {
    project: Option<PathBuf>,
    candidates: Vec<Candidate>,
}

impl Snapshot {
    pub async fn discover(project: &Path) -> Self {
        let Ok(project) = project.canonicalize() else {
            return Self::default();
        };
        let candidates = discover_project(&project).await;
        Self {
            project: Some(project),
            candidates,
        }
    }

    /// Return frozen candidates only for commands in the original project.
    /// Drop any destination that no longer resolves to the approved spelling.
    pub fn candidates_for(&self, cwd: &Path) -> impl Iterator<Item = &Candidate> {
        let in_project = self
            .project
            .as_ref()
            .is_some_and(|root| cwd.canonicalize().is_ok_and(|cwd| cwd.starts_with(root)));
        self.candidates.iter().filter(move |candidate| {
            in_project
                && output_directory(&candidate.directory)
                    .as_ref()
                    .is_some_and(|path| path == &candidate.directory)
        })
    }
}

/// Inspect the exact project manifest, without applying mise project configuration.
async fn discover_project(root: &Path) -> Vec<Candidate> {
    let Some(manifest) = read_config(root, "mise.toml") else {
        return Vec::new();
    };
    let Some(tools) = manifest.get("tools").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    let Some(executable) = which::which("mise")
        .ok()
        .and_then(|path| path.canonicalize().ok())
    else {
        return Vec::new();
    };
    if executable.starts_with(root) {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    for (tool, keys, binary, destination) in [
        (
            Tool::Cargo,
            &["rust"][..],
            "cargo",
            cargo::destination(root),
        ),
        (
            Tool::Mbx,
            &["github:jdx/mr-boxington", "mr-boxington"][..],
            "mbx",
            mbx::destination(root),
        ),
    ] {
        let Some(directory) = destination else {
            continue;
        };
        let Some((key, version)) = keys.iter().find_map(|key| {
            let value = tools.get(*key)?;
            let version = value.as_str().or_else(|| value.get("version")?.as_str())?;
            Some((*key, version))
        }) else {
            continue;
        };
        if mise::installed(&executable, key, version, binary).await {
            candidates.push(Candidate { tool, directory });
        }
    }
    candidates
}

fn read_config(root: &Path, name: &str) -> Option<toml::Table> {
    const MAX_CONFIG_BYTES: u64 = 128 * 1024;
    let path = root.join(name);
    // Do not follow configuration symlinks, including a symlinked .cargo folder.
    if path.canonicalize().ok()? != path || !path.is_file() {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > MAX_CONFIG_BYTES {
        return None;
    }
    text.parse().ok()
}

fn directory(root: &Path, setting: &str) -> Option<PathBuf> {
    if setting.is_empty() {
        return None;
    }
    output_directory(&root.join(setting))
}

fn output_directory(path: &Path) -> Option<PathBuf> {
    let directory = path.canonicalize().ok()?;
    (directory.is_dir() && directory.parent().is_some()).then_some(directory)
}
