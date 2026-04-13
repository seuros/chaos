//! Chat transcript entry types and streaming match helpers.

use std::path::PathBuf;

use iced::widget::markdown;

/// A single entry in the transcript.
///
/// Refactored in #15 from "string + role enum" into a tagged enum so each
/// kind of event can render with its own widget (markdown for agent output,
/// collapsed shell transcripts for exec commands, structured labels for
/// errors, etc.).
#[derive(Debug)]
pub(super) enum ChatEntry {
    /// Raw user input the composer submitted. Always plain text.
    User { text: String },
    /// Agent reply. Rendered as markdown — streaming content deltas push
    /// into [`markdown::Content`] incrementally so the user watches text
    /// appear as it arrives.
    Agent { content: markdown::Content },
    /// Reasoning summary. Structurally identical to an agent message but
    /// rendered muted so the user can tell them apart.
    Reasoning { content: markdown::Content },
    /// A shell command the agent ran.
    Exec {
        command: Vec<String>,
        cwd: PathBuf,
        /// `None` while the command is still running, `Some(code)` after
        /// `ExecCommandEnd` lands.
        exit_code: Option<i32>,
        /// Aggregated stdout/stderr preview. Empty until the command ends.
        output: String,
    },
    /// An MCP tool call. Mirrors [`ChatEntry::Exec`] but for non-shell tools.
    Tool {
        server: String,
        tool: String,
        /// `None` while the call is in flight, `Some(Ok(summary))` on
        /// success, `Some(Err(msg))` on failure. Modeled as `Result` (not
        /// a raw `String`) so the view arm can discriminate without
        /// string sniffing on the rendered text.
        result: Option<Result<String, String>>,
    },
    /// A structured kernel notice that doesn't belong in any of the rich
    /// categories above. Level drives the muted/warn/error styling.
    Notice { level: NoticeLevel, text: String },
}

/// Severity for a [`ChatEntry::Notice`]. Three levels is plenty for the
/// scaffold — #16 can layer real theming on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoticeLevel {
    /// Informational: background events, deprecation notices, disconnect logs.
    Info,
    /// Non-fatal: warnings, retry notices, stream errors.
    Warn,
    /// Terminal errors from the kernel.
    Error,
}

/// Tiny trait used by `finalize_in_place` to parameterize over "which
/// streaming entry kind am I finalizing". Keeps the tail-scan loop in
/// one place instead of duplicating it per variant.
pub(super) trait StreamMatch {
    fn matches(entry: &ChatEntry) -> bool;
    fn rebuild(full: &str) -> ChatEntry;
}

pub(super) struct MatchAgent;
impl StreamMatch for MatchAgent {
    fn matches(entry: &ChatEntry) -> bool {
        matches!(entry, ChatEntry::Agent { .. })
    }
    fn rebuild(full: &str) -> ChatEntry {
        ChatEntry::Agent {
            content: markdown::Content::parse(full),
        }
    }
}

pub(super) struct MatchReasoning;
impl StreamMatch for MatchReasoning {
    fn matches(entry: &ChatEntry) -> bool {
        matches!(entry, ChatEntry::Reasoning { .. })
    }
    fn rebuild(full: &str) -> ChatEntry {
        ChatEntry::Reasoning {
            content: markdown::Content::parse(full),
        }
    }
}
