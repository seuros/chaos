//! Retained rows for scrolling the chat pane without opening a pager.

use std::sync::{Arc, Weak};

use crate::chatwidget::ActiveCellTranscriptKey;
use crate::history_cell::HistoryCell;
use crate::transcript_reflow::reflow_transcript_lines;
use crate::wrapping::word_wrap_lines;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

/// Navigation within the retained chat output.
pub enum Scroll {
    Up(usize),
    Down(usize),
    Start,
    End,
}

/// Cached main-screen output and a reading position independent of the live tail.
/// Reset this state when the displayed process or its history is cleared.
#[derive(Default)]
pub struct ChatScrollback {
    /// An absolute row, rather than distance from the bottom: incoming output
    /// must not move what the operator is reading. None follows live output.
    top: Option<usize>,
    history_key: Option<(u16, usize)>,
    history_tail: Option<Weak<dyn HistoryCell>>,
    history: Vec<Line<'static>>,
    live_key: Option<(u16, Option<ActiveCellTranscriptKey>)>,
    live: Vec<Line<'static>>,
}

impl ChatScrollback {
    pub fn is_scrolled(&self) -> bool {
        self.top.is_some()
    }

    pub fn follow_live(&mut self) {
        self.top = None;
    }

    /// Refresh compact history and live output only when their cache keys change.
    pub fn sync(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        width: u16,
        active_key: Option<ActiveCellTranscriptKey>,
        live_lines: impl FnOnce(u16) -> Vec<Line<'static>>,
    ) {
        let width = width.max(1);
        let tail = cells.last().map(Arc::downgrade);
        let same_tail = match (&self.history_tail, &tail) {
            (Some(previous), Some(current)) => Weak::ptr_eq(previous, current),
            (None, None) => true,
            _ => false,
        };
        let history_key = (width, cells.len());
        if self.history_key != Some(history_key) || !same_tail {
            // Use the main view's compact display lines, not the transcript
            // viewer's expanded tool output. Wrap before counting screen rows.
            self.history = word_wrap_lines(
                reflow_transcript_lines(cells, width, None),
                usize::from(width),
            );
            self.history_key = Some(history_key);
            self.history_tail = tail;
        }
        let live_key = (width, active_key);
        if self.live_key != Some(live_key) {
            self.live = word_wrap_lines(live_lines(width), usize::from(width));
            if !self.live.is_empty() {
                // Match the active cell's top padding in ChatWidget.
                self.live.insert(0, Line::default());
            }
            self.live_key = Some(live_key);
        }
    }

    fn max_top(&self, height: u16) -> usize {
        (self.history.len() + self.live.len()).saturating_sub(usize::from(height))
    }

    pub fn scroll(&mut self, scroll: Scroll, height: u16) {
        let max = self.max_top(height);
        let top = self.top.unwrap_or(max).min(max);
        let next = match scroll {
            Scroll::Up(rows) => top.saturating_sub(rows),
            Scroll::Down(rows) => top.saturating_add(rows).min(max),
            Scroll::Start => 0,
            Scroll::End => max,
        };
        self.top = (height > 0 && next < max).then_some(next);
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let top = self
            .top
            .unwrap_or(self.max_top(area.height))
            .min(self.max_top(area.height));
        if self.top.is_some() {
            self.top = Some(top);
        }
        let lines = self
            .history
            .iter()
            .chain(&self.live)
            .skip(top)
            .take(usize::from(area.height))
            .cloned()
            .collect::<Vec<_>>();
        Paragraph::new(lines).render(area, buf);
    }
}

#[cfg(test)]
mod tests;
