use crate::history_cell::HistoryCell;
use crate::history_cell::{self};
use crate::render::line_utils::prefix_lines;
use crate::style::proposed_plan_style;
use ratatui::prelude::Stylize;
use ratatui::text::Line;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use super::StreamState;

/// Controller that manages newline-gated streaming, header emission, and
/// commit animation across streams.
pub struct StreamController {
    state: StreamState,
    finishing_after_drain: bool,
    header_emitted: bool,
}

impl StreamController {
    /// Create a controller whose markdown renderer shortens local file links relative to `cwd`.
    ///
    /// The controller snapshots the path into stream state so later commit ticks and finalization
    /// render against the same session cwd that was active when streaming started.
    pub fn new(width: Option<usize>, cwd: &Path) -> Self {
        Self {
            state: StreamState::new(width, cwd),
            finishing_after_drain: false,
            header_emitted: false,
        }
    }

    /// Push a delta; if it completes lines and a render is due, commit them and start animation.
    pub fn push(&mut self, delta: &str) -> bool {
        self.state.push_and_maybe_commit(delta)
    }

    /// Finalize the active stream. Drain and emit now.
    pub fn finalize(&mut self) -> Option<Box<dyn HistoryCell>> {
        // Finalize collector first.
        let remaining = {
            let state = &mut self.state;
            state.collector.finalize_and_drain()
        };
        // Collect all output first to avoid emitting headers when there is no content.
        let mut out_lines = Vec::new();
        {
            let state = &mut self.state;
            if !remaining.is_empty() {
                state.enqueue(remaining);
            }
            let step = state.drain_all();
            out_lines.extend(step);
        }

        // Cleanup
        self.state.clear();
        self.finishing_after_drain = false;
        self.emit(out_lines)
    }

    /// Step animation: commit at most one queued line and handle end-of-drain cleanup.
    pub fn on_commit_tick(&mut self) -> (Option<Box<dyn HistoryCell>>, bool) {
        self.state.commit_if_due(Instant::now());
        let step = self.state.step();
        (self.emit(step), self.is_idle())
    }

    /// Step animation: commit at most `max_lines` queued lines.
    ///
    /// This is intended for adaptive catch-up drains. Callers should keep `max_lines` bounded; a
    /// very large value can collapse perceived animation into a single jump.
    pub fn on_commit_tick_batch(
        &mut self,
        max_lines: usize,
    ) -> (Option<Box<dyn HistoryCell>>, bool) {
        self.state.commit_if_due(Instant::now());
        let step = self.state.drain_n(max_lines.max(1));
        (self.emit(step), self.is_idle())
    }

    /// Idle only when no lines are queued and no throttled render is waiting,
    /// so a pending render keeps commit ticks alive until it flushes.
    fn is_idle(&self) -> bool {
        self.state.is_idle() && !self.state.has_pending_render()
    }

    /// Returns the current number of queued lines waiting to be displayed.
    pub fn queued_lines(&self) -> usize {
        self.state.queued_len()
    }

    /// Returns whether a throttled markdown render has not been flushed yet.
    pub fn has_pending_render(&self) -> bool {
        self.state.has_pending_render()
    }

    /// Returns the age of the oldest queued line.
    pub fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        self.state.oldest_queued_age(now)
    }

    fn emit(&mut self, lines: Vec<Line<'static>>) -> Option<Box<dyn HistoryCell>> {
        if lines.is_empty() {
            return None;
        }
        Some(Box::new(history_cell::AgentMessageCell::new(lines, {
            let header_emitted = self.header_emitted;
            self.header_emitted = true;
            !header_emitted
        })))
    }
}

/// Controller that streams proposed plan markdown into a styled plan block.
pub struct PlanStreamController {
    state: StreamState,
    header_emitted: bool,
    top_padding_emitted: bool,
}

impl PlanStreamController {
    /// Create a plan-stream controller whose markdown renderer shortens local file links relative
    /// to `cwd`.
    ///
    /// The controller snapshots the path into stream state so later commit ticks and finalization
    /// render against the same session cwd that was active when streaming started.
    pub fn new(width: Option<usize>, cwd: &Path) -> Self {
        Self {
            state: StreamState::new(width, cwd),
            header_emitted: false,
            top_padding_emitted: false,
        }
    }

    /// Push a delta; if it completes lines and a render is due, commit them and start animation.
    pub fn push(&mut self, delta: &str) -> bool {
        self.state.push_and_maybe_commit(delta)
    }

    /// Finalize the active stream. Drain and emit now.
    pub fn finalize(&mut self) -> Option<Box<dyn HistoryCell>> {
        let remaining = {
            let state = &mut self.state;
            state.collector.finalize_and_drain()
        };
        let mut out_lines = Vec::new();
        {
            let state = &mut self.state;
            if !remaining.is_empty() {
                state.enqueue(remaining);
            }
            let step = state.drain_all();
            out_lines.extend(step);
        }

        self.state.clear();
        self.emit(out_lines, /*include_bottom_padding*/ true)
    }

    /// Step animation: commit at most one queued line and handle end-of-drain cleanup.
    pub fn on_commit_tick(&mut self) -> (Option<Box<dyn HistoryCell>>, bool) {
        self.state.commit_if_due(Instant::now());
        let step = self.state.step();
        (
            self.emit(step, /*include_bottom_padding*/ false),
            self.is_idle(),
        )
    }

    /// Step animation: commit at most `max_lines` queued lines.
    ///
    /// This is intended for adaptive catch-up drains. Callers should keep `max_lines` bounded; a
    /// very large value can collapse perceived animation into a single jump.
    pub fn on_commit_tick_batch(
        &mut self,
        max_lines: usize,
    ) -> (Option<Box<dyn HistoryCell>>, bool) {
        self.state.commit_if_due(Instant::now());
        let step = self.state.drain_n(max_lines.max(1));
        (
            self.emit(step, /*include_bottom_padding*/ false),
            self.is_idle(),
        )
    }

    /// Idle only when no lines are queued and no throttled render is waiting,
    /// so a pending render keeps commit ticks alive until it flushes.
    fn is_idle(&self) -> bool {
        self.state.is_idle() && !self.state.has_pending_render()
    }

    /// Returns the current number of queued plan lines waiting to be displayed.
    pub fn queued_lines(&self) -> usize {
        self.state.queued_len()
    }

    /// Returns whether a throttled markdown render has not been flushed yet.
    pub fn has_pending_render(&self) -> bool {
        self.state.has_pending_render()
    }

    /// Returns the age of the oldest queued plan line.
    pub fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        self.state.oldest_queued_age(now)
    }

    fn emit(
        &mut self,
        lines: Vec<Line<'static>>,
        include_bottom_padding: bool,
    ) -> Option<Box<dyn HistoryCell>> {
        if lines.is_empty() && !include_bottom_padding {
            return None;
        }

        let mut out_lines: Vec<Line<'static>> = Vec::new();
        let is_stream_continuation = self.header_emitted;
        if !self.header_emitted {
            out_lines.push(vec!["• ".dim(), "Proposed Plan".bold()].into());
            out_lines.push(Line::from(" "));
            self.header_emitted = true;
        }

        let mut plan_lines: Vec<Line<'static>> = Vec::new();
        if !self.top_padding_emitted {
            plan_lines.push(Line::from(" "));
            self.top_padding_emitted = true;
        }
        plan_lines.extend(lines);
        if include_bottom_padding {
            plan_lines.push(Line::from(" "));
        }

        let plan_style = proposed_plan_style();
        let plan_lines = prefix_lines(plan_lines, "  ".into(), "  ".into())
            .into_iter()
            .map(|line| line.style(plan_style))
            .collect::<Vec<_>>();
        out_lines.extend(plan_lines);

        Some(Box::new(history_cell::new_proposed_plan_stream(
            out_lines,
            is_stream_continuation,
        )))
    }
}

#[cfg(test)]
pub(crate) mod tests;
