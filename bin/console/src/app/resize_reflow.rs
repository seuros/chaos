//! Rebuilds terminal scrollback from transcript cells when the terminal width changes.
//!
//! History reaches the terminal as already-wrapped rows, so a narrower or wider window leaves every
//! previously written line wrapped for a width that is gone. The cells are still in memory, so the
//! repair is to clear what we wrote, re-render the cells at the new width, and write them back.
//!
//! The rebuild is debounced: dragging a window edge emits a burst of intermediate widths, and
//! repainting the whole transcript for each one is both slow and ugly. It is also capped, because
//! replaying more rows than the terminal retains is work nobody can scroll back to see.

use std::time::Instant;

use libui::transcript_reflow::ReflowRowCap;
use libui::transcript_reflow::TRANSCRIPT_REFLOW_DEBOUNCE;
use libui::transcript_reflow::reflow_transcript_lines;
use ratatui::layout::Size;

use super::{App, Result, tui};

impl App {
    fn reflow_row_cap(&self) -> ReflowRowCap {
        ReflowRowCap::Auto
    }

    /// Forget any queued rebuild along with the transcript it would have replayed.
    pub(crate) fn reset_transcript_reflow(&mut self) {
        self.transcript_reflow.clear();
    }

    /// Note the terminal size for this draw and queue a rebuild if the width moved.
    ///
    /// Only width changes are repaired here. A height change moves rows around the inline viewport
    /// but leaves their wrapping intact, and the viewport anchoring in `Tui::draw` already handles
    /// that case.
    pub(super) fn handle_draw_pre_render(&mut self, tui: &mut tui::Tui, size: Size) -> Result<()> {
        let last_known_screen_size = tui.terminal.last_known_screen_size;
        if size != last_known_screen_size {
            self.refresh_status_line();
        }

        let width = self.transcript_reflow.note_width(size.width);
        if width.changed && self.transcript_reflow.reflow_needed_for_width(size.width) {
            // Rows queued before the resize are wrapped for the old width; the rebuild renders them
            // again from source.
            tui.clear_pending_history_lines();
            self.transcript_reflow.schedule_debounced(Some(size.width));
            tui.frame_requester()
                .schedule_frame_in(TRANSCRIPT_REFLOW_DEBOUNCE);
        }

        self.maybe_run_resize_reflow(tui, size)
    }

    /// Run a queued rebuild once its quiet period has passed.
    ///
    /// An overlay owns the whole screen while it is open, so a rebuild waits for it to close rather
    /// than writing scrollback underneath it.
    fn maybe_run_resize_reflow(&mut self, tui: &mut tui::Tui, size: Size) -> Result<()> {
        let Some(deadline) = self.transcript_reflow.pending_until() else {
            return Ok(());
        };
        let now = Instant::now();
        if now < deadline {
            // Later resizes push the deadline out while the frame scheduler coalesces delayed draws
            // to the earliest request, so re-arm the draw or the rebuild waits for a keypress.
            tui.frame_requester().schedule_frame_in(deadline - now);
            return Ok(());
        }
        if self.overlay.is_some() || tui.is_alt_screen_active() {
            return Ok(());
        }

        self.transcript_reflow.clear_pending_reflow();
        self.reflow_transcript_now(tui, size)?;
        self.transcript_reflow.mark_reflowed_width(size.width);
        Ok(())
    }

    /// Clear the rows we own and write the transcript back at the current width.
    fn reflow_transcript_now(&mut self, tui: &mut tui::Tui, size: Size) -> Result<()> {
        let lines = reflow_transcript_lines(
            &self.transcript_cells,
            size.width,
            self.reflow_row_cap().max_rows(),
        );

        tui.clear_pending_history_lines();
        self.deferred_history_lines.clear();
        tui.terminal.clear_scrollback_and_visible_screen_ansi()?;

        // The rebuild starts from an empty screen, so the viewport goes back to the top of the
        // scrollable region and grows downwards as history is inserted above it.
        let reserved = tui.top_reserved_rows();
        let mut area = tui.terminal.viewport_area;
        area.y = reserved;
        area.width = size.width;
        area.height = area.height.min(size.height.saturating_sub(reserved));
        tui.terminal.set_viewport_area(area);
        // The resize is handled: tell the terminal so the draw that follows does not also try to
        // reposition the viewport for a size change this rebuild already absorbed.
        tui.terminal.resize(size)?;

        self.has_emitted_history_lines = !lines.is_empty();
        if !lines.is_empty() {
            tui.insert_history_lines(lines);
        }
        Ok(())
    }
}
