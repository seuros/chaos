use chaos_pwd::find_chaos_home;
use std::fs::OpenOptions;
use std::fs::{self};

pub(crate) const DEBUG_LOG_PATH_ENV_VAR: &str = "CHAOS_DEBUG_LOG_PATH";

/// Prepares debug logging by truncating `~/.chaos/debug.log`, exporting its path
/// via `CHAOS_DEBUG_LOG_PATH`, and printing the operator hint.
///
/// The actual tracing layer must be attached by the concrete runtime
/// (`chaos-console`, `chaos-fork`, direct login flow, etc.) so it composes with
/// that runtime's existing subscriber setup instead of competing with it.
pub(crate) fn prepare_debug_logging() -> anyhow::Result<()> {
    let chaos_home = find_chaos_home()?;

    // Ensure the chaos home directory exists.
    fs::create_dir_all(&chaos_home)?;

    let log_path = chaos_home.join("debug.log");

    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .map_err(|e| anyhow::anyhow!("failed to open {}: {e}", log_path.display()))?;

    // SAFETY: this runs during one-shot CLI bootstrap before we start any
    // background worker threads for the current command.
    unsafe {
        std::env::set_var(DEBUG_LOG_PATH_ENV_VAR, &log_path);
    }

    eprintln!("debug: logging to {}", log_path.display());
    Ok(())
}
