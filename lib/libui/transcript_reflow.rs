//! Scheduling state for repairing transcript scrollback after a terminal resize.
//!
//! Terminal scrollback is not a retained widget tree. Once wrapped rows are written out, the
//! terminal owns them, and a width change leaves them wrapped for a width that no longer exists.
//! The repair treats the in-memory history cells as the source of truth: clear the rows we wrote,
//! re-render the cells at the new width, write them again.
//!
//! This module owns only the scheduling half of that lifecycle — when a rebuild is due and how many
//! rows it is worth rebuilding. It does not know how to render a cell or how to clear a terminal;
//! the app loop consumes this state and performs the rebuild.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use chaos_kern::terminal::TerminalName;
use chaos_kern::terminal::terminal_info;
use ratatui::text::Line;

use crate::history_cell::HistoryCell;

/// Quiet period a resize must survive before scrollback is rebuilt.
///
/// Dragging a terminal edge produces a burst of intermediate sizes. Rebuilding on each one would
/// repaint the whole transcript dozens of times for widths the user never stops at.
pub const TRANSCRIPT_REFLOW_DEBOUNCE: Duration = Duration::from_millis(75);

const VSCODE_REFLOW_MAX_ROWS: usize = 1_000;
const WEZTERM_REFLOW_MAX_ROWS: usize = 3_500;
const ALACRITTY_REFLOW_MAX_ROWS: usize = 10_000;
const FALLBACK_REFLOW_MAX_ROWS: usize = 2_000;

/// How many rows a rebuild is allowed to write back into scrollback.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReflowRowCap {
    /// Derive the cap from the detected terminal's documented scrollback default.
    #[default]
    Auto,
    /// Rebuild the entire transcript however long it is.
    Unlimited,
    /// Rebuild at most this many rows, keeping the newest.
    Limit(usize),
}

impl ReflowRowCap {
    /// Resolve the cap to a row count, or `None` when every row should be rebuilt.
    pub fn max_rows(self) -> Option<usize> {
        match self {
            Self::Auto => Some(auto_max_rows(terminal_info().name)),
            Self::Unlimited => None,
            Self::Limit(max_rows) => Some(max_rows),
        }
    }
}

/// Rows worth replaying for a terminal, based on its documented scrollback default.
///
/// Replaying more rows than the terminal retains is work nobody can scroll back to see. The match
/// is exhaustive so a new terminal variant has to make a deliberate choice here.
fn auto_max_rows(terminal_name: TerminalName) -> usize {
    match terminal_name {
        TerminalName::VsCode => VSCODE_REFLOW_MAX_ROWS,
        TerminalName::WezTerm => WEZTERM_REFLOW_MAX_ROWS,
        TerminalName::Alacritty => ALACRITTY_REFLOW_MAX_ROWS,
        TerminalName::AppleTerminal
        | TerminalName::Ghostty
        | TerminalName::Iterm2
        | TerminalName::WarpTerminal
        | TerminalName::Kitty
        | TerminalName::Konsole
        | TerminalName::GnomeTerminal
        | TerminalName::Vte
        | TerminalName::Dumb
        | TerminalName::Unknown => FALLBACK_REFLOW_MAX_ROWS,
    }
}

/// Tracks whether transcript scrollback still needs repair, and when.
///
/// Observed width and rebuilt width are deliberately separate. A terminal can report an
/// intermediate size during a drag, settle on the final size only after the rebuild has already
/// run, and then never send another resize. Keeping the two apart lets the next draw notice that
/// the settled width was never actually rendered and ask for one more pass.
#[derive(Debug, Default)]
pub struct TranscriptReflowState {
    last_observed_width: Option<u16>,
    last_reflow_width: Option<u16>,
    pending_reflow_width: Option<u16>,
    pending_until: Option<Instant>,
}

impl TranscriptReflowState {
    /// Forget all width and deadline state.
    ///
    /// Call this when the transcript a pending rebuild would have replayed is discarded. A stale
    /// deadline left behind would rebuild history from unrelated cells.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Record the width seen during a draw and report how it relates to the last one.
    ///
    /// The first width observed only initialises the baseline: nothing has been written at another
    /// width yet, so there is nothing to repair.
    pub fn note_width(&mut self, width: u16) -> TranscriptWidthChange {
        let previous_width = self.last_observed_width.replace(width);
        if previous_width.is_none() {
            self.last_reflow_width = Some(width);
        }
        TranscriptWidthChange {
            changed: previous_width.is_some_and(|previous| previous != width),
            initialized: previous_width.is_none(),
        }
    }

    /// Return whether scrollback still has to be rebuilt at `width`.
    ///
    /// The comparison is against the width that actually rebuilt scrollback and the width a rebuild
    /// is already queued for — not merely the last width a draw happened to see.
    pub fn reflow_needed_for_width(&self, width: u16) -> bool {
        self.last_reflow_width != Some(width) && self.pending_reflow_width != Some(width)
    }

    /// Queue a rebuild once the terminal has been quiet for [`TRANSCRIPT_REFLOW_DEBOUNCE`].
    ///
    /// Each call pushes the deadline out, so a drag rebuilds once at the width the user released
    /// on. `target_width` is `None` for a height-only change, which needs a rebuild but must not
    /// claim to have handled a width the renderer never saw.
    pub fn schedule_debounced(&mut self, target_width: Option<u16>) {
        if let Some(target_width) = target_width {
            self.pending_reflow_width = Some(target_width);
        }
        self.pending_until = Some(Instant::now() + TRANSCRIPT_REFLOW_DEBOUNCE);
    }

    /// The instant a queued rebuild becomes due, if one is queued.
    pub fn pending_until(&self) -> Option<Instant> {
        self.pending_until
    }

    /// Whether a rebuild is queued and its quiet period has elapsed.
    pub fn pending_is_due(&self, now: Instant) -> bool {
        self.pending_until.is_some_and(|deadline| now >= deadline)
    }

    /// Drop a queued rebuild without running it.
    pub fn clear_pending_reflow(&mut self) {
        self.pending_until = None;
        self.pending_reflow_width = None;
    }

    /// Record the width that actually rebuilt scrollback.
    pub fn mark_reflowed_width(&mut self, width: u16) {
        self.last_reflow_width = Some(width);
    }
}

/// Render history cells into the rows a rebuild should write back to scrollback.
///
/// Rendering walks backwards from the newest cell so a capped rebuild never formats a backlog it
/// is going to throw away. If the retained suffix starts inside a run of stream continuations, the
/// walk keeps going until it has the cell that opened the run: a continuation carries no separator
/// of its own, so starting mid-run would glue the tail of one message onto the previous one.
///
/// The blank-line separators match what the live insert path emits, so a rebuilt transcript is
/// spaced the same as one that was written a cell at a time.
pub fn reflow_transcript_lines(
    cells: &[Arc<dyn HistoryCell>],
    width: u16,
    max_rows: Option<usize>,
) -> Vec<Line<'static>> {
    let mut rendered: VecDeque<(Vec<Line<'static>>, bool)> = VecDeque::new();
    let mut rendered_rows = 0usize;
    let mut start = cells.len();

    while start > 0 {
        start -= 1;
        let cell = &cells[start];
        let lines = cell.display_lines(width);
        // The separator this cell may need is counted too, so the cap is not overshot by the
        // spacing that gets added back below.
        rendered_rows += lines.len() + 1;
        rendered.push_front((lines, cell.is_stream_continuation()));
        if max_rows.is_some_and(|max_rows| rendered_rows > max_rows) {
            break;
        }
    }

    while start > 0
        && rendered
            .front()
            .is_some_and(|(_, is_continuation)| *is_continuation)
    {
        start -= 1;
        let cell = &cells[start];
        rendered.push_front((cell.display_lines(width), cell.is_stream_continuation()));
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut has_emitted = false;
    for (cell_lines, is_continuation) in rendered {
        if cell_lines.is_empty() {
            continue;
        }
        if !is_continuation {
            if has_emitted {
                lines.push(Line::from(""));
            } else {
                has_emitted = true;
            }
        }
        lines.extend(cell_lines);
    }

    if let Some(max_rows) = max_rows
        && lines.len() > max_rows
    {
        lines = lines.split_off(lines.len() - max_rows);
    }
    lines
}

/// How the width seen by the latest draw relates to the previous one.
pub struct TranscriptWidthChange {
    /// A different width had been observed before this one.
    pub changed: bool,
    /// This was the first width the state machine ever saw.
    pub initialized: bool,
}

#[cfg(test)]
pub(crate) mod tests;
