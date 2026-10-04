//! Interpreter-independent child-process fixtures for execution and sandbox tests.
//!
//! Scripts run in a standalone binary with vendored Lua, not in the test parent:
//! the child's terminal, inherited descriptors, and OS sandbox remain observable.

use std::path::PathBuf;

/// Locate (or build) the shared fixture binary using the workspace's test helper.
pub fn executable() -> Result<PathBuf, chaos_which::CargoBinError> {
    chaos_which::cargo_bin("chaos-test-process")
}

/// An argv vector which can be passed directly to a process API or any wrapper.
pub fn command(script: &str) -> Result<Vec<String>, chaos_which::CargoBinError> {
    Ok(vec![
        executable()?.to_string_lossy().into_owned(),
        "--eval".to_string(),
        script.to_string(),
    ])
}

/// Quote the fixture's argv for shell-based tool tests, without a heredoc.
pub fn shell_command(script: &str) -> std::io::Result<String> {
    quote_argv(&command(script).map_err(std::io::Error::other)?)
}

/// Start a persistent Lua REPL, optionally evaluating a startup script first.
pub fn interactive_shell_command(script: &str) -> std::io::Result<String> {
    let mut args = command(script).map_err(std::io::Error::other)?;
    args.push("--interactive".to_string());
    quote_argv(&args)
}

fn quote_argv(args: &[String]) -> std::io::Result<String> {
    shlex::try_join(args.iter().map(String::as_str))
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))
}
