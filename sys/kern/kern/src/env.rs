//! Functions for environment detection that need to be shared across crates.

use chaos_sysinfo::env;

/// Returns true when ChaOS is likely running headless (CI, SSH, no display).
///
/// Used by frontends to skip flows that require a browser (e.g. device-code auth).
pub fn is_headless_environment() -> bool {
    if env::is_set("CI") || chaos_sysinfo::terminal::is_ssh_session() {
        return true;
    }

    #[cfg(target_os = "linux")]
    {
        if chaos_sysinfo::session_display_server().is_none() {
            return true;
        }
    }

    false
}
