use std::io::ErrorKind;
use std::path::Path;

use crate::SESSIONS_SUBDIR;

/// Inspect an `anyhow::Error` from session initialization and produce a
/// human-readable diagnostic when the root cause is a recognizable I/O error.
///
/// Returns `Some(message)` when a friendly hint could be built, `None` when the
/// error is not an I/O kind we handle (callers should fall back to a generic
/// message).
pub fn diagnose_session_init_error(err: &anyhow::Error, codex_home: &Path) -> Option<String> {
    err.chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .find_map(|io_err| diagnose_io_error(io_err, codex_home))
}

fn diagnose_io_error(io_err: &std::io::Error, codex_home: &Path) -> Option<String> {
    let sessions_dir = codex_home.join(SESSIONS_SUBDIR);
    let hint = match io_err.kind() {
        ErrorKind::PermissionDenied => format!(
            "Codex cannot access session files at {} (permission denied). \
             If sessions were created using sudo, fix ownership: \
             sudo chown -R $(whoami) {}",
            sessions_dir.display(),
            codex_home.display()
        ),
        ErrorKind::NotFound => format!(
            "Session storage missing at {}. \
             Create the directory or choose a different Codex home.",
            sessions_dir.display()
        ),
        ErrorKind::AlreadyExists => format!(
            "Session storage path {} is blocked by an existing file. \
             Remove or rename it so Codex can create sessions.",
            sessions_dir.display()
        ),
        ErrorKind::InvalidData | ErrorKind::InvalidInput => format!(
            "Session data under {} looks corrupt or unreadable. \
             Clearing the sessions directory may help \
             (this will remove saved threads).",
            sessions_dir.display()
        ),
        ErrorKind::IsADirectory | ErrorKind::NotADirectory => format!(
            "Session storage path {} has an unexpected type. \
             Ensure it is a directory Codex can use for session files.",
            sessions_dir.display()
        ),
        _ => return None,
    };

    Some(format!("{hint} (underlying error: {io_err})"))
}
