//! Script discovery — finds script files from config layer directories.
//!
//! Scripts are loaded from well-known paths in lexicographic order:
//!   1. `~/.config/chaos/scripts/*`  (user layer)
//!   2. `.chaos/scripts/*`           (project layer — higher precedence)
//!
//! Supported extensions: `.lua`, `.wasm`

use std::path::Path;
use std::path::PathBuf;

/// Known script extensions and their engine backends.
const SCRIPT_EXTENSIONS: &[&str] = &["lua", "wasm"];

/// Discover scripts from the standard config directories.
///
/// Returns paths sorted lexicographically within each layer.
/// Project-layer scripts come after user-layer scripts so they
/// can override registrations.
///
/// `user_scripts_dir_override` — when `Some`, replaces the XDG user-layer
/// directory. Pass an empty directory (or a non-existent path) to suppress
/// user scripts entirely, e.g. in tests.
pub fn discover_scripts(cwd: &Path, user_scripts_dir_override: Option<&Path>) -> Vec<PathBuf> {
    let mut scripts = Vec::new();

    // User layer: ~/.config/chaos/scripts/ (or override)
    let user_dir = match user_scripts_dir_override {
        Some(dir) => Some(dir.to_path_buf()),
        None => dirs().map(|d| d.join("scripts")),
    };
    if let Some(dir) = user_dir {
        collect_script_files(&dir, &mut scripts);
    }

    // Project layer: .chaos/scripts/
    collect_script_files(&cwd.join(".chaos").join("scripts"), &mut scripts);

    scripts
}

/// Discover scripts filtered to a specific extension.
pub fn discover_scripts_by_ext(
    cwd: &Path,
    ext: &str,
    user_scripts_dir_override: Option<&Path>,
) -> Vec<PathBuf> {
    discover_scripts(cwd, user_scripts_dir_override)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == ext))
        .collect()
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

/// Return the Chaos config directory (`~/.config/chaos`).
fn dirs() -> Option<PathBuf> {
    dirs_helper::config_dir().map(|d| d.join("chaos"))
}

/// Thin wrapper around the `dirs` crate logic.
mod dirs_helper {
    use std::path::PathBuf;

    pub fn config_dir() -> Option<PathBuf> {
        // Respect XDG_CONFIG_HOME, fall back to ~/.config
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            let p = PathBuf::from(xdg);
            if p.is_absolute() {
                return Some(p);
            }
        }
        home_dir().map(|h| h.join(".config"))
    }

    fn home_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discovers_project_scripts() {
        let tmp = tempfile::tempdir().unwrap();
        let scripts_dir = tmp.path().join(".chaos").join("scripts");
        fs::create_dir_all(&scripts_dir).unwrap();

        fs::write(scripts_dir.join("b_second.lua"), "-- second").unwrap();
        fs::write(scripts_dir.join("a_first.lua"), "-- first").unwrap();
        fs::write(scripts_dir.join("not_lua.txt"), "-- ignored").unwrap();

        // Pass a non-existent path as the user-layer override so real user
        // scripts (e.g. ~/.config/chaos/scripts/hello.lua) are never loaded.
        let found = discover_scripts(tmp.path(), Some(&tmp.path().join("no_user_scripts")));
        let names: Vec<&str> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();

        assert_eq!(names, vec!["a_first.lua", "b_second.lua"]);
    }

    #[test]
    fn missing_dir_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let found = discover_scripts(tmp.path(), Some(&tmp.path().join("no_user_scripts")));
        assert!(found.is_empty());
    }
}
