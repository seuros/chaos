use chaos_realpath::AbsolutePathBuf;
use std::path::PathBuf;

/// Resolve an installed mbx's cache using host settings only.
pub fn mbx_cache() -> Option<AbsolutePathBuf> {
    which::which("mbx").ok()?;
    let home = std::env::home_dir()?;
    let (config, cache) = if cfg!(target_os = "macos") {
        (
            home.join("Library/Application Support"),
            home.join("Library/Caches"),
        )
    } else if cfg!(target_os = "linux") {
        let xdg = |key, default| {
            std::env::var_os(key)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(default))
        };
        (
            xdg("XDG_CONFIG_HOME", ".config"),
            xdg("XDG_CACHE_HOME", ".cache"),
        )
    } else {
        return None;
    };
    // Never derive an outside write grant from a checkout's .mbx.toml or target symlink.
    let root = match std::env::var_os("MBX_CACHE_DIR") {
        Some(path) => PathBuf::from(path),
        None => match std::fs::read_to_string(config.join("mbx/config.toml")) {
            Ok(text) => match text.parse::<toml::Table>().ok()?.get("cache_dir") {
                Some(value) => PathBuf::from(value.as_str()?),
                None => cache.join("mbx"),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => cache.join("mbx"),
            Err(_) => return None,
        },
    };
    if !root.is_absolute() || !root.is_dir() {
        return None;
    }
    let root = root.canonicalize().ok()?;
    if home.canonicalize().ok()?.starts_with(&root) {
        return None;
    }
    AbsolutePathBuf::from_absolute_path(root).ok()
}
