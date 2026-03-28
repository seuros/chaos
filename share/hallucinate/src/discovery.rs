//! Script discovery — finds `.lua` files from config layer directories.
//!
//! Scripts are loaded from well-known paths in lexicographic order:
//!   1. `~/.config/chaos/scripts/*.lua`  (user layer)
//!   2. `.chaos/scripts/*.lua`           (project layer — higher precedence)

use std::path::{Path, PathBuf};

/// Discover Lua scripts from the standard config directories.
///
/// Returns paths sorted lexicographically within each layer.
/// Project-layer scripts come after user-layer scripts so they
/// can override registrations.
pub fn discover_scripts(cwd: &Path) -> Vec<PathBuf> {
    let mut scripts = Vec::new();

    // User layer: ~/.config/chaos/scripts/
    if let Some(config_dir) = dirs() {
        collect_lua_files(&config_dir.join("scripts"), &mut scripts);
    }

    // Project layer: .chaos/scripts/
    collect_lua_files(&cwd.join(".chaos").join("scripts"), &mut scripts);

    scripts
}

/// Collect all `.lua` files from a directory, sorted by name.
fn collect_lua_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let mut lua_files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "lua"))
        .filter(|p| p.is_file())
        .collect();

    lua_files.sort();
    out.extend(lua_files);
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

        let found = discover_scripts(tmp.path());
        let names: Vec<&str> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();

        assert_eq!(names, vec!["a_first.lua", "b_second.lua"]);
    }

    #[test]
    fn missing_dir_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let found = discover_scripts(tmp.path());
        assert!(found.is_empty());
    }
}
