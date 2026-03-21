use std::path::Path;
use std::path::PathBuf;

pub(crate) fn system_cache_root_dir(codex_home: &Path) -> PathBuf {
    codex_home.join("skills").join(".system")
}

pub(crate) fn install_system_skills(_codex_home: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

pub(crate) fn uninstall_system_skills(_codex_home: &Path) {
    // no-op
}
