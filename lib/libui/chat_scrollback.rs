//! Retained rows for scrolling the chat pane without opening a pager.

use std::sync::{Arc, Weak};

use crate::chatwidget::ActiveCellTranscriptKey;
use crate::history_cell::HistoryCell;
use crate::mermaid::{DisplayChunk, MermaidRenderer};
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
    graphics_active: bool,
    mermaid: Option<MermaidRenderer>,
    diagrams: Vec<DiagramPlacement>,
}

struct DiagramPlacement {
    source: String,
    row: usize,
    rows: usize,
    is_image: bool,
}

impl ChatScrollback {
    pub fn is_scrolled(&self) -> bool {
        self.top.is_some()
    }

    /// The retained viewport owns graphics.
    pub fn uses_retained_view(&self) -> bool {
        self.is_scrolled() || self.graphics_active
    }

    pub fn prepare_graphics(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        requester: crate::tui::FrameRequester,
        picker: Option<&ratatui_image::picker::Picker>,
    ) {
        let has_mermaid = cells.iter().any(|cell| cell.has_mermaid());
        if !has_mermaid {
            self.mermaid = None;
        }
        if has_mermaid
            && self.mermaid.is_none()
            && let Some(picker) = picker
        {
            self.mermaid = Some(MermaidRenderer::new(picker.clone(), requester));
        }
        let active = has_mermaid && self.mermaid.is_some();
        if self.graphics_active != active {
            self.history_key = None;
        }
        self.graphics_active = active;
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
        if self
            .mermaid
            .as_mut()
            .is_some_and(|renderer| renderer.refresh(width))
        {
            self.history_key = None;
        }
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
            if self.graphics_active {
                self.rebuild_graphical_history(cells, width);
            } else {
                self.history = word_wrap_lines(
                    reflow_transcript_lines(cells, width, None),
                    usize::from(width),
                );
                self.diagrams.clear();
            }
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

    fn rebuild_graphical_history(&mut self, cells: &[Arc<dyn HistoryCell>], width: u16) {
        self.history.clear();
        self.diagrams.clear();
        let mut has_emitted = false;
        for cell in cells {
            if !cell.has_display_content() {
                continue;
            }
            if !cell.is_stream_continuation() {
                if has_emitted {
                    self.history.push(Line::default());
                }
                has_emitted = true;
            }
            for chunk in cell.display_chunks(width) {
                match chunk {
                    DisplayChunk::Text(lines) => {
                        self.history
                            .extend(word_wrap_lines(lines, usize::from(width)));
                    }
                    DisplayChunk::Mermaid { source, fallback } => {
                        let image = self.mermaid.as_ref().and_then(|r| r.image(&source));
                        let is_image = image.is_some();
                        if is_image {
                            let prefix = if fallback
                                .first()
                                .is_some_and(|line| line.to_string().starts_with('•'))
                            {
                                "•"
                            } else {
                                " "
                            };
                            self.history
                                .push(Line::from(format!("{prefix} Mermaid diagram")));
                        }
                        let row = self.history.len();
                        if let Some(image) = image {
                            self.history.extend(std::iter::repeat_n(
                                Line::default(),
                                usize::from(image.size().height),
                            ));
                        } else {
                            self.history
                                .extend(word_wrap_lines(fallback, usize::from(width)));
                        }
                        self.diagrams.push(DiagramPlacement {
                            source,
                            row,
                            rows: self.history.len() - row,
                            is_image,
                        });
                    }
                }
            }
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

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, graphics_visible: bool) {
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
        // The Kitty graphics protocol (also used by Ghostty) marks a
        // transmission as sent when it writes into the buffer.
        // Delay transmission until the palette clears so the payload reaches
        // the terminal intact.
        if !graphics_visible {
            return;
        }
        if let Some(renderer) = &mut self.mermaid {
            let bottom = top.saturating_add(usize::from(area.height));
            let visible = self.diagrams.iter().filter(|diagram| {
                diagram.row < bottom && diagram.row.saturating_add(diagram.rows) > top
            });
            renderer.begin_frame(visible.clone().map(|diagram| diagram.source.as_str()));
            for diagram in visible {
                renderer.request(&diagram.source);
                if diagram.is_image
                    && let Some(image) = renderer.image(&diagram.source)
                {
                    crate::mermaid::render_slice(
                        &image,
                        diagram.row as i64 - top as i64,
                        area,
                        buf,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
