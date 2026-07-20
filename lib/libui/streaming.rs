//! Streaming primitives used by the TUI transcript pipeline.
//!
//! `StreamState` owns newline-gated markdown collection and a FIFO queue of committed render lines.
//! Higher-level modules build on top of this state:
//! - `controller` adapts queued lines into `HistoryCell` emission rules for message and plan streams.
//! - `chunking` computes adaptive drain plans from queue pressure.
//! - `commit_tick` binds policy decisions to concrete controller drains.
//!
//! The key invariant is queue ordering. All drains pop from the front, and enqueue records an
//! arrival timestamp so policy code can reason about oldest queued age without peeking into text.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use ratatui::text::Line;

use crate::markdown_stream::MarkdownStreamCollector;
pub mod chunking;
pub mod commit_tick;
pub mod controller;

struct QueuedLine {
    line: Line<'static>,
    enqueued_at: Instant,
}

/// Minimum interval between full markdown re-renders while lines are already
/// queued for display; display pacing stays with the queue.
const MARKDOWN_RENDER_INTERVAL: Duration = Duration::from_millis(47);

/// Holds in-flight markdown stream state and queued committed lines.
pub struct StreamState {
    pub collector: MarkdownStreamCollector,
    queued_lines: VecDeque<QueuedLine>,
    pub has_seen_delta: bool,
    render_pending: bool,
    last_render_at: Option<Instant>,
}

impl StreamState {
    /// Create stream state whose markdown collector renders local file links relative to `cwd`.
    ///
    /// Controllers are expected to pass the session cwd here once and keep it stable for the
    /// lifetime of the active stream.
    pub fn new(width: Option<usize>, cwd: &Path) -> Self {
        Self {
            collector: MarkdownStreamCollector::new(width, cwd),
            queued_lines: VecDeque::new(),
            has_seen_delta: false,
            render_pending: false,
            last_render_at: None,
        }
    }
    /// Resets collector and queue state for the next stream lifecycle.
    pub fn clear(&mut self) {
        self.collector.clear();
        self.queued_lines.clear();
        self.has_seen_delta = false;
        self.render_pending = false;
        self.last_render_at = None;
    }
    /// Accumulates a delta and commits newly completed lines when a render is due.
    ///
    /// Returns true when new lines were enqueued.
    pub fn push_and_maybe_commit(&mut self, delta: &str) -> bool {
        if !delta.is_empty() {
            self.has_seen_delta = true;
        }
        self.collector.push_delta(delta);
        if delta.contains('\n') {
            self.render_pending = true;
        }
        self.commit_if_due(Instant::now())
    }
    /// Renders pending completed lines when the throttle allows it; an empty
    /// queue always renders immediately. Returns true when lines were enqueued.
    pub fn commit_if_due(&mut self, now: Instant) -> bool {
        if !self.render_pending {
            return false;
        }
        let due = self.queued_lines.is_empty()
            || self
                .last_render_at
                .is_none_or(|at| now.saturating_duration_since(at) >= MARKDOWN_RENDER_INTERVAL);
        if !due {
            return false;
        }
        self.render_pending = false;
        self.last_render_at = Some(now);
        let newly_completed = self.collector.commit_complete_lines();
        if newly_completed.is_empty() {
            return false;
        }
        self.enqueue(newly_completed);
        true
    }
    /// Returns whether a throttled render is still waiting to run.
    pub fn has_pending_render(&self) -> bool {
        self.render_pending
    }
    /// Drains one queued line from the front of the queue.
    pub fn step(&mut self) -> Vec<Line<'static>> {
        self.queued_lines
            .pop_front()
            .map(|queued| queued.line)
            .into_iter()
            .collect()
    }
    /// Drains up to `max_lines` queued lines from the front of the queue.
    ///
    /// Callers that pass very large values still get bounded behavior because this method clamps to
    /// the currently available queue length.
    pub fn drain_n(&mut self, max_lines: usize) -> Vec<Line<'static>> {
        let end = max_lines.min(self.queued_lines.len());
        self.queued_lines
            .drain(..end)
            .map(|queued| queued.line)
            .collect()
    }
    /// Drains all queued lines from the front of the queue.
    pub fn drain_all(&mut self) -> Vec<Line<'static>> {
        self.queued_lines
            .drain(..)
            .map(|queued| queued.line)
            .collect()
    }
    /// Returns whether no lines are queued for commit.
    pub fn is_idle(&self) -> bool {
        self.queued_lines.is_empty()
    }
    /// Returns the current queue depth.
    pub fn queued_len(&self) -> usize {
        self.queued_lines.len()
    }
    /// Returns the age of the oldest queued line.
    pub fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        self.queued_lines
            .front()
            .map(|queued| now.saturating_duration_since(queued.enqueued_at))
    }
    /// Appends committed lines to the queue with a shared enqueue timestamp.
    pub fn enqueue(&mut self, lines: Vec<Line<'static>>) {
        let now = Instant::now();
        self.queued_lines
            .extend(lines.into_iter().map(|line| QueuedLine {
                line,
                enqueued_at: now,
            }));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    fn test_cwd() -> PathBuf {
        // These tests only need a stable absolute cwd; using temp_dir() avoids baking Unix- or
        // Windows-specific root semantics into the fixtures.
        std::env::temp_dir()
    }

    pub(crate) async fn streaming_suite() {
        super::chunking::tests::streaming_chunking_suite();
        super::controller::tests::controller_loose_vs_tight_with_commit_ticks_matches_full().await;
        drain_n_clamps_to_available_lines();
    }

    fn drain_n_clamps_to_available_lines() {
        let mut state = StreamState::new(None, &test_cwd());
        state.enqueue(vec![Line::from("one")]);

        let drained = state.drain_n(8);
        assert_eq!(drained, vec![Line::from("one")]);
        assert!(state.is_idle());
    }
}
