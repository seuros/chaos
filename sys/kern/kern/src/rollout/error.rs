use std::io::ErrorKind;
use std::path::Path;

use crate::error::CodexErr;

pub(crate) fn map_session_init_error(err: &anyhow::Error, codex_home: &Path) -> CodexErr {
    match diagnose_session_init_error(err, codex_home) {
        Some(message) => CodexErr::Fatal(message),
        None => CodexErr::Fatal(format!("Failed to initialize session: {err:#}")),
    }
}

fn diagnose_session_init_error(err: &anyhow::Error, codex_home: &Path) -> Option<String> {
    err.chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .find_map(|io_err| diagnose_io_error(io_err, codex_home))
}

fn diagnose_io_error(io_err: &std::io::Error, codex_home: &Path) -> Option<String> {
    let hint = match io_err.kind() {
        ErrorKind::PermissionDenied => format!(
            "ChaOS cannot access persisted session storage under {} (permission denied). \
             If session state was created using sudo, fix ownership: \
             sudo chown -R $(whoami) {}",
            codex_home.display(),
            codex_home.display()
        ),
        ErrorKind::NotFound => format!(
            "Persisted session storage is missing under {}. \
             Create the directory or choose a different ChaOS home.",
            codex_home.display()
        ),
        ErrorKind::AlreadyExists => format!(
            "A required session-storage path under {} is blocked by an existing file. \
             Remove or rename it so ChaOS can continue.",
            codex_home.display()
        ),
        ErrorKind::InvalidData | ErrorKind::InvalidInput => format!(
            "Persisted session state under {} looks corrupt or unreadable.",
            codex_home.display()
        ),
        ErrorKind::IsADirectory | ErrorKind::NotADirectory => format!(
            "A persisted-session storage path under {} has an unexpected type.",
            codex_home.display()
        ),
        _ => return None,
    };

    Some(format!("{hint} (underlying error: {io_err})"))
}
