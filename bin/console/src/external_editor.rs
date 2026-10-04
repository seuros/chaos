use std::env;
use std::fs;
use std::process::Stdio;

use color_eyre::eyre::Report;
use color_eyre::eyre::Result;
use tempfile::Builder;
use thiserror::Error;
use tokio::process::Command;

mod lifecycle;
use lifecycle::{EditorInvocation, EditorInvocationEvent, EditorProcess, EditorResources};

#[derive(Debug, Error)]
pub(crate) enum EditorError {
    #[error("neither VISUAL nor EDITOR is set")]
    MissingEditor,
    #[error("failed to parse editor command")]
    ParseFailed,
    #[error("editor command is empty")]
    EmptyCommand,
}

/// Resolve the editor command from environment variables.
/// Prefers `VISUAL` over `EDITOR`.
pub(crate) fn resolve_editor_command() -> std::result::Result<Vec<String>, EditorError> {
    let raw = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .map_err(|_| EditorError::MissingEditor)?;
    let parts = shlex::split(&raw).ok_or(EditorError::ParseFailed)?;
    if parts.is_empty() {
        return Err(EditorError::EmptyCommand);
    }
    Ok(parts)
}

/// Write `seed` to a temp file, launch the editor command, and return the updated content.
pub(crate) async fn run_editor(seed: &str, editor_cmd: &[String]) -> Result<String> {
    if editor_cmd.is_empty() {
        return Err(Report::msg("editor command is empty"));
    }

    // Convert to TempPath immediately so no file handle stays open on Windows.
    let temp_path = Builder::new().suffix(".md").tempfile()?.into_temp_path();
    fs::write(&temp_path, seed)?;

    let mut cmd = Command::new(&editor_cmd[0]);
    if editor_cmd.len() > 1 {
        cmd.args(&editor_cmd[1..]);
    }
    let child = cmd
        .arg(&temp_path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let mut invocation = EditorInvocation::new(()).into_dynamic();
    assert!(
        invocation
            .handle(EditorInvocationEvent::Launch(Some(EditorProcess::new(
                EditorResources {
                    path: temp_path,
                    child,
                }
            ))))
            .is_ok()
    );

    let resources = invocation
        .running_data_mut()
        .unwrap_or_else(|| unreachable!("launched editor owns resources"))
        .resources_mut();
    let result = async {
        let status = resources.child.wait().await?;
        if !status.success() {
            return Err(Report::msg(format!("editor exited with status {status}")));
        }
        Ok(fs::read_to_string(&resources.path)?)
    }
    .await;

    assert!(invocation.handle(EditorInvocationEvent::Finish).is_ok());
    result
}

#[cfg(test)]
pub(crate) mod tests;
