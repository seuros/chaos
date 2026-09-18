use chaos_ipc::protocol::SandboxPolicy;
use std::path::PathBuf;

/// Returns a warning describing why `--add-dir` entries will be ignored for the
/// resolved sandbox policy. The caller is responsible for presenting the
/// warning to the user (for example, printing to stderr).
pub fn add_dir_warning_message(
    additional_dirs: &[PathBuf],
    sandbox_policy: &SandboxPolicy,
) -> Option<String> {
    if additional_dirs.is_empty() {
        return None;
    }

    match sandbox_policy {
        SandboxPolicy::WorkspaceWrite { .. }
        | SandboxPolicy::RootAccess
        | SandboxPolicy::ExternalSandbox { .. } => None,
        SandboxPolicy::ReadOnly { .. } => Some(format_warning(additional_dirs)),
    }
}

fn format_warning(additional_dirs: &[PathBuf]) -> String {
    let joined_paths = additional_dirs
        .iter()
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Ignoring --add-dir ({joined_paths}) because the effective sandbox mode is read-only. Switch to workspace-write or root-access to allow additional writable roots."
    )
}

#[cfg(test)]
pub(crate) mod tests;
