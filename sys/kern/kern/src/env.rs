//! Functions for environment detection that need to be shared across crates.

fn env_var_set(key: &str) -> bool {
    std::env::var(key).is_ok_and(|v| !v.trim().is_empty())
}

/// Returns true when ChaOS is likely running headless (CI, SSH, no display).
///
/// Used by frontends to skip flows that require a browser (e.g. device-code auth).
pub fn is_headless_environment() -> bool {
    if env_var_set("CI")
        || env_var_set("SSH_CONNECTION")
        || env_var_set("SSH_CLIENT")
        || env_var_set("SSH_TTY")
    {
        return true;
    }

    #[cfg(target_os = "linux")]
    {
        if !env_var_set("DISPLAY") && !env_var_set("WAYLAND_DISPLAY") {
            return true;
        }
    }

    false
}
