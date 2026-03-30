use std::io::ErrorKind;
use std::path::Path;

use crate::error::ChaosErr;

pub(crate) fn map_session_init_error(err: &anyhow::Error, chaos_home: &Path) -> ChaosErr {
    match diagnose_session_init_error(err, chaos_home) {
        Some(message) => ChaosErr::Fatal(message),
        None => ChaosErr::Fatal(format!("Failed to initialize session: {err:#}")),
    }
}

fn diagnose_session_init_error(err: &anyhow::Error, chaos_home: &Path) -> Option<String> {
    err.chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .find_map(|io_err| diagnose_io_error(io_err, chaos_home))
}

fn diagnose_io_error(io_err: &std::io::Error, chaos_home: &Path) -> Option<String> {
    let hint = match io_err.kind() {
        ErrorKind::PermissionDenied => format!(
            "ChaOS cannot access persisted session storage under {} (permission denied). \
             If session state was created using sudo, fix ownership: \
             sudo chown -R $(whoami) {}",
            chaos_home.display(),
            chaos_home.display()
        ),
        ErrorKind::NotFound => format!(
            "Persisted session storage is missing under {}. \
             Create the directory or choose a different ChaOS home.",
            chaos_home.display()
        ),
        ErrorKind::AlreadyExists => format!(
            "A required session-storage path under {} is blocked by an existing file. \
             Remove or rename it so ChaOS can continue.",
            chaos_home.display()
        ),
        ErrorKind::InvalidData | ErrorKind::InvalidInput => format!(
            "Persisted session state under {} looks corrupt or unreadable.",
            chaos_home.display()
        ),
        ErrorKind::IsADirectory | ErrorKind::NotADirectory => format!(
            "A persisted-session storage path under {} has an unexpected type.",
            chaos_home.display()
        ),
        _ => return None,
    };

    Some(format!("{hint} (underlying error: {io_err})"))
}
