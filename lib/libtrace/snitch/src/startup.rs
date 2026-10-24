//! Local startup checkpoints. No prompts, paths, credentials, or network probes.
//!
//! Frontends may start recording before installing tracing, then replay those
//! checkpoints with `enable_logging`. Separate timelines keep UI readiness and
//! kernel readiness from being mistaken for the same measurement.

use crate::tracing;
use jiff::SignedDuration;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::time::Instant;

#[derive(Clone, Debug)]
pub struct StartupTimeline {
    scope: &'static str,
    start: Instant,
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    checkpoints: Vec<Checkpoint>,
    emitted: usize,
    logging: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Checkpoint {
    stage: &'static str,
    elapsed: SignedDuration,
    duration: SignedDuration,
}

impl StartupTimeline {
    pub fn new(scope: &'static str) -> Self {
        Self {
            scope,
            start: Instant::now(),
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    /// Record a milestone once, including across clones.
    pub fn mark(&self, stage: &'static str) {
        self.record(
            stage,
            SignedDuration::try_from(self.start.elapsed()).unwrap_or(SignedDuration::MAX),
        );
        self.flush();
    }

    pub fn enable_logging(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .logging = true;
        self.flush();
    }

    fn record(&self, stage: &'static str, elapsed: SignedDuration) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.checkpoints.iter().any(|point| point.stage == stage) {
            return;
        }
        let previous = state
            .checkpoints
            .last()
            .map(|point| point.elapsed)
            .unwrap_or_default();
        let elapsed = elapsed.max(previous);
        state.checkpoints.push(Checkpoint {
            stage,
            elapsed,
            duration: elapsed.saturating_sub(previous),
        });
    }

    fn flush(&self) {
        let pending = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.logging {
                return;
            }
            let pending = state.checkpoints[state.emitted..].to_vec();
            state.emitted = state.checkpoints.len();
            pending
        };
        for point in pending {
            tracing::info!(
                target: "chaos_snitch::startup",
                scope = self.scope,
                stage = point.stage,
                elapsed_ms = point.elapsed.as_secs_f64() * 1000.0,
                duration_ms = point.duration.as_secs_f64() * 1000.0,
                "startup checkpoint"
            );
        }
    }
}

#[cfg(test)]
mod tests;
