use std::path::{Path, PathBuf};

pub(super) fn destination(root: &Path) -> Option<PathBuf> {
    let config = super::read_config(root, ".mbx.toml")?;
    super::directory(root, config.get("cache_dir")?.as_str()?)
}
