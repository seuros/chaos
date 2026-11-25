use std::path::{Path, PathBuf};

pub(super) fn destination(root: &Path) -> Option<PathBuf> {
    // Cargo prefers the extensionless file when both files exist.
    let name = if root.join(".cargo/config").symlink_metadata().is_ok() {
        ".cargo/config"
    } else {
        ".cargo/config.toml"
    };
    let config = super::read_config(root, name)?;
    super::directory(root, config.get("build")?.get("target-dir")?.as_str()?)
}
