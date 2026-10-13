//! Folders that carry non-config assets (`rules/*.decrees`, `agents/*`).
//!
//! Configuration, the user's rules, and the user's roles live in the runtime
//! database. These folders only anchor file-based assets and are ordered from
//! lowest to highest precedence: administrator, then trusted project folders
//! from root to cwd.
use chaos_realpath::AbsolutePathBuf;
use std::path::PathBuf;

pub const RULES_DIR_NAME: &str = "rules";
pub const AGENTS_DIR_NAME: &str = "agents";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetScope {
    /// `/etc/chaos`; the only scope whose rules may grant authority.
    Admin,
    /// `<dir>/.chaos` between cwd and the project root; trust-gated, restrictions only.
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetFolder {
    pub scope: AssetScope,
    pub dir: AbsolutePathBuf,
    /// Present when the folder exists but must not be loaded (untrusted project).
    pub disabled_reason: Option<String>,
}

impl AssetFolder {
    pub fn new(scope: AssetScope, dir: AbsolutePathBuf) -> Self {
        Self {
            scope,
            dir,
            disabled_reason: None,
        }
    }

    pub fn disabled(scope: AssetScope, dir: AbsolutePathBuf, reason: impl Into<String>) -> Self {
        Self {
            scope,
            dir,
            disabled_reason: Some(reason.into()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.disabled_reason.is_none()
    }

    pub fn rules_dir(&self) -> PathBuf {
        self.dir.as_path().join(RULES_DIR_NAME)
    }

    pub fn agents_dir(&self) -> PathBuf {
        self.dir.as_path().join(AGENTS_DIR_NAME)
    }
}
