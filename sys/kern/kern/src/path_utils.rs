use std::path::Path;
use std::path::PathBuf;

// Re-export filesystem utilities from codex-config.
pub use chaos_sysctl::path_utils::SymlinkWritePaths;
pub use chaos_sysctl::path_utils::resolve_symlink_write_paths;
pub use chaos_sysctl::path_utils::write_atomically;

pub fn normalize_for_path_comparison(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    path.as_ref().canonicalize()
}

pub fn normalize_for_native_workdir(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref().to_path_buf()
}
