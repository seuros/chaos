use std::path::PathBuf;

use chaos_realpath::AbsolutePathBuf;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(tag = "kind")]
pub enum VfsSpecialPath {
    Root,
    Minimal,
    CurrentWorkingDirectory,
    ProjectRoots {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        subpath: Option<PathBuf>,
    },
    Tmpdir,
    SlashTmp,
    /// WARNING: `:special_path` tokens are part of config compatibility.
    /// Do not make older runtimes reject newly introduced tokens.
    /// New parser support should be additive, while unknown values must stay
    /// representable so config from a newer Chaos degrades to warn-and-ignore
    /// instead of failing to load. Chaos 0.112.0 rejected unknown values here,
    /// which broke forward compatibility for newer config.
    /// Preserves future special-path tokens so older runtimes can ignore them
    /// without rejecting config authored by a newer release.
    Unknown {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        subpath: Option<PathBuf>,
    },
}

impl VfsSpecialPath {
    pub fn project_roots(subpath: Option<PathBuf>) -> Self {
        Self::ProjectRoots { subpath }
    }

    pub fn unknown(path: impl Into<String>, subpath: Option<PathBuf>) -> Self {
        Self::Unknown {
            path: path.into(),
            subpath,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
pub enum VfsPath {
    Path { path: AbsolutePathBuf },
    Special { value: VfsSpecialPath },
}
