//! Terminal detection: emulator, multiplexer, and SSH transport.

use serde::Serialize;

#[cfg(feature = "tui")]
mod emulator;

#[cfg(feature = "tui")]
pub use emulator::{TerminalInfo, TerminalName, terminal_info, user_agent};

/// Detected terminal multiplexer metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Multiplexer {
    /// tmux terminal multiplexer.
    Tmux {
        /// tmux version string when `TERM_PROGRAM=tmux` is available.
        ///
        /// This is derived from `TERM_PROGRAM_VERSION`.
        version: Option<String>,
        pane: Option<String>,
    },
    /// zellij terminal multiplexer.
    Zellij { pane: Option<String> },
    /// GNU screen.
    Screen { session: Option<String> },
}

impl Multiplexer {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Tmux { .. } => "tmux",
            Self::Zellij { .. } => "zellij",
            Self::Screen { .. } => "screen",
        }
    }

    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Tmux { pane, .. } | Self::Zellij { pane } => pane.as_deref(),
            Self::Screen { session } => session.as_deref(),
        }
    }
}

/// Environment variable access used by terminal detection.
///
/// This trait exists to allow faking the environment in tests.
trait Environment {
    /// Returns an environment variable when set.
    fn var(&self, name: &str) -> Option<String>;

    /// Returns a non-empty environment variable.
    fn var_non_empty(&self, name: &str) -> Option<String> {
        self.var(name).and_then(none_if_whitespace)
    }

    /// Returns whether an environment variable is set and non-empty.
    fn has_non_empty(&self, name: &str) -> bool {
        self.var_non_empty(name).is_some()
    }
}

/// Reads environment variables from the running process.
struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn var(&self, name: &str) -> Option<String> {
        crate::env::var(name)
    }
}

pub fn multiplexer() -> Option<Multiplexer> {
    detect_multiplexer(&ProcessEnvironment)
}

pub fn is_ssh_session() -> bool {
    detect_ssh_session(&ProcessEnvironment)
}

fn detect_multiplexer(env: &dyn Environment) -> Option<Multiplexer> {
    if env.has_non_empty("TMUX") || env.has_non_empty("TMUX_PANE") {
        return Some(Multiplexer::Tmux {
            version: tmux_version_from_env(env),
            pane: env.var_non_empty("TMUX_PANE"),
        });
    }

    if env.has_non_empty("ZELLIJ")
        || env.has_non_empty("ZELLIJ_SESSION_NAME")
        || env.has_non_empty("ZELLIJ_VERSION")
    {
        return Some(Multiplexer::Zellij {
            pane: env.var_non_empty("ZELLIJ_PANE_ID"),
        });
    }

    if env.has_non_empty("STY") {
        return Some(Multiplexer::Screen {
            session: env.var_non_empty("STY"),
        });
    }

    None
}

fn detect_ssh_session(env: &dyn Environment) -> bool {
    env.has_non_empty("SSH_CONNECTION")
        || env.has_non_empty("SSH_CLIENT")
        || env.has_non_empty("SSH_TTY")
}

fn is_tmux_term_program(value: &str) -> bool {
    value.eq_ignore_ascii_case("tmux")
}

fn tmux_version_from_env(env: &dyn Environment) -> Option<String> {
    let term_program = env.var("TERM_PROGRAM")?;
    if !is_tmux_term_program(&term_program) {
        return None;
    }

    env.var_non_empty("TERM_PROGRAM_VERSION")
}

fn none_if_whitespace(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

#[cfg(test)]
mod tests;
