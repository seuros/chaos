use std::path::Path;
use std::path::PathBuf;

use crate::env;

// Re-export filesystem utilities from codex-config.
pub use chaos_sysctl::path_utils::SymlinkWritePaths;
pub use chaos_sysctl::path_utils::resolve_symlink_write_paths;
pub use chaos_sysctl::path_utils::write_atomically;

pub fn normalize_for_path_comparison(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let canonical = path.as_ref().canonicalize()?;
    Ok(normalize_for_wsl(canonical))
}

pub fn normalize_for_native_workdir(path: impl AsRef<Path>) -> PathBuf {
    normalize_for_native_workdir_with_flag(path.as_ref().to_path_buf(), false)
}

fn normalize_for_wsl(path: PathBuf) -> PathBuf {
    normalize_for_wsl_with_flag(path, env::is_wsl())
}

fn normalize_for_native_workdir_with_flag(path: PathBuf, is_windows: bool) -> PathBuf {
    if is_windows {
        path.to_path_buf()
    } else {
        path
    }
}

fn normalize_for_wsl_with_flag(path: PathBuf, is_wsl: bool) -> PathBuf {
    if !is_wsl {
        return path;
    }

    if !is_wsl_case_insensitive_path(&path) {
        return path;
    }

    lower_ascii_path(path)
}

fn is_wsl_case_insensitive_path(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::path::Component;

        let mut components = path.components();
        let Some(Component::RootDir) = components.next() else {
            return false;
        };
        let Some(Component::Normal(mnt)) = components.next() else {
            return false;
        };
        if !ascii_eq_ignore_case(mnt.as_bytes(), b"mnt") {
            return false;
        }
        let Some(Component::Normal(drive)) = components.next() else {
            return false;
        };
        let drive_bytes = drive.as_bytes();
        drive_bytes.len() == 1 && drive_bytes[0].is_ascii_alphabetic()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

#[cfg(target_os = "linux")]
fn ascii_eq_ignore_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(lhs, rhs)| lhs.to_ascii_lowercase() == *rhs)
}

#[cfg(target_os = "linux")]
fn lower_ascii_path(path: PathBuf) -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::ffi::OsStringExt;

    let bytes = path.as_os_str().as_bytes();
    let mut lowered = Vec::with_capacity(bytes.len());
    for byte in bytes {
        lowered.push(byte.to_ascii_lowercase());
    }
    PathBuf::from(OsString::from_vec(lowered))
}

#[cfg(not(target_os = "linux"))]
fn lower_ascii_path(path: PathBuf) -> PathBuf {
    path
}

#[cfg(test)]
#[path = "path_utils_tests.rs"]
mod tests;
