use std::path::Path;

use crate::error::CodexErr;

pub(crate) fn map_session_init_error(err: &anyhow::Error, codex_home: &Path) -> CodexErr {
    match chaos_rollout::error::diagnose_session_init_error(err, codex_home) {
        Some(message) => CodexErr::Fatal(message),
        None => CodexErr::Fatal(format!("Failed to initialize session: {err:#}")),
    }
}
