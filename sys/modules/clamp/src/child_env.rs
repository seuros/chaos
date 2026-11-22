use tokio::process::Command;

/// Session identity and IPC a running Claude Code hands its children. A
/// clamped `claude` started from inside a Claude Code session must not
/// believe it is that session's child, nor hold its messaging credentials.
const INHERITED_SESSION_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_CHILD_NOTES_DIR",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
];

/// Remove what no Claude Code child may inherit: a parent session's identity
/// and IPC, and the host's own daemon-only names.
pub(crate) fn strip(cmd: &mut Command, env_remove: &[String]) {
    for name in INHERITED_SESSION_ENV {
        cmd.env_remove(name);
    }
    for name in env_remove {
        cmd.env_remove(name);
    }
}

/// The environment clamp protocol v2 gives every Claude Code child. Call it
/// after every other override: removals run last, so a name the host forbids
/// never reaches the child, whoever set it.
pub(crate) fn apply(
    cmd: &mut Command,
    env_remove: &[String],
    stream_idle_timeout: Option<std::time::Duration>,
) {
    // Chaos owns every tool call: a resumed session must not run one on its own.
    cmd.env("CLAUDE_CODE_DISABLE_DEFERRED_TOOL_RESUME", "1");
    if let Some(timeout) = stream_idle_timeout {
        cmd.env(
            "CLAUDE_STREAM_IDLE_TIMEOUT_MS",
            timeout.as_millis().to_string(),
        );
    }
    strip(cmd, env_remove);
}

#[cfg(test)]
mod tests;
