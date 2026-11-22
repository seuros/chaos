use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

use crate::transport::ClampError;

/// The oldest Claude Code clamp protocol v2 drives.
pub const MIN_CLAUDE_CODE: Version = Version(2, 1, 330);

const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// A Claude Code release, `major.minor.patch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u32, pub u32, pub u32);

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// The release in `claude --version` output, such as `2.1.330 (Claude Code)`.
pub fn parse(output: &str) -> Option<Version> {
    let word = output.split_whitespace().next()?;
    let mut parts = word.split('.').map(str::parse::<u32>);
    let version = Version(
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    parts.next().is_none().then_some(version)
}

/// Refuse a Claude Code older than [`MIN_CLAUDE_CODE`]. The binary is asked
/// on every spawn, from the directory the child will run in: a version-manager
/// shim resolves per directory and per pin, so no earlier answer is trusted.
pub async fn ensure_supported(
    cli: &Path,
    cwd: Option<&Path>,
    env_remove: &[String],
) -> Result<Version, ClampError> {
    let mut probe = Command::new(cli);
    probe.arg("--version").kill_on_drop(true);
    if let Some(cwd) = cwd {
        probe.current_dir(cwd);
    }
    crate::child_env::strip(&mut probe, env_remove);
    let output = tokio::time::timeout(VERSION_PROBE_TIMEOUT, probe.output())
        .await
        .map_err(|_| unsupported(cli, "no answer to --version"))?
        .map_err(|error| unsupported(cli, &format!("cannot run --version: {error}")))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(version) = parse(&text) else {
        return Err(unsupported(
            cli,
            &format!("unrecognized --version output {:?}", text.trim()),
        ));
    };
    if version < MIN_CLAUDE_CODE {
        return Err(unsupported(cli, &format!("found {version}")));
    }
    Ok(version)
}

fn unsupported(cli: &Path, found: &str) -> ClampError {
    ClampError::UnsupportedClaudeCode {
        cli: cli.display().to_string(),
        found: found.to_owned(),
    }
}

#[cfg(test)]
mod tests;
