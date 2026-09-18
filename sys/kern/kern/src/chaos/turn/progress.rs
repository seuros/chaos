use std::time::Duration;
use std::time::Instant;

use chaos_ipc::protocol::TurnProgressEvent;

const EMIT_INTERVAL: Duration = Duration::from_millis(500);
const APPROX_BYTES_PER_TOKEN: usize = 4;
const APPROX_SILENT_THINKING_TOKENS_PER_SECOND: i64 = 25;

/// Provider-neutral, UI-only live progress estimator for a user-visible turn.
///
/// This tracker exists solely to produce a transient liveness indicator while
/// the stream is quiet or producing deltas.
#[derive(Debug)]
pub(super) struct TurnProgressTracker {
    output_bytes: usize,
    reasoning_bytes: usize,
    max_silent_reasoning_tokens: i64,
    last_emitted_total_tokens: i64,
    last_emit_at: Instant,
    last_stream_activity_at: Instant,
}

impl TurnProgressTracker {
    pub(super) fn new() -> Self {
        Self::new_at(Instant::now())
    }

    fn new_at(now: Instant) -> Self {
        // Allow the first non-zero observation to emit immediately.
        Self {
            output_bytes: 0,
            reasoning_bytes: 0,
            max_silent_reasoning_tokens: 0,
            last_emitted_total_tokens: 0,
            last_emit_at: now - EMIT_INTERVAL,
            last_stream_activity_at: now,
        }
    }

    pub(super) fn emit_interval() -> Duration {
        EMIT_INTERVAL
    }

    pub(super) fn observe_output_delta(&mut self, delta: &str) {
        self.observe_output_delta_at(delta, Instant::now());
    }

    pub(super) fn observe_reasoning_delta(&mut self, delta: &str) {
        self.observe_reasoning_delta_at(delta, Instant::now());
    }

    fn observe_output_delta_at(&mut self, delta: &str, now: Instant) {
        self.remember_silent_reasoning_until(now);
        self.output_bytes = self.output_bytes.saturating_add(delta.len());
        self.last_stream_activity_at = now;
    }

    fn observe_reasoning_delta_at(&mut self, delta: &str, now: Instant) {
        self.remember_silent_reasoning_until(now);
        self.reasoning_bytes = self.reasoning_bytes.saturating_add(delta.len());
        self.last_stream_activity_at = now;
    }

    pub(super) fn event_if_due(&mut self, turn_id: &str) -> Option<TurnProgressEvent> {
        self.event_if_due_at(turn_id, Instant::now())
    }

    fn event_if_due_at(&mut self, turn_id: &str, now: Instant) -> Option<TurnProgressEvent> {
        if now.duration_since(self.last_emit_at) < EMIT_INTERVAL {
            return None;
        }

        self.remember_silent_reasoning_until(now);

        let observed_reasoning_tokens = approx_tokens_from_bytes(self.reasoning_bytes);
        let approx_reasoning_tokens =
            observed_reasoning_tokens.max(self.max_silent_reasoning_tokens);
        let approx_output_tokens = approx_tokens_from_bytes(self.output_bytes);
        let approx_total_tokens = approx_reasoning_tokens + approx_output_tokens;
        if approx_total_tokens == 0 || approx_total_tokens == self.last_emitted_total_tokens {
            return None;
        }

        self.last_emit_at = now;
        self.last_emitted_total_tokens = approx_total_tokens;
        Some(TurnProgressEvent {
            turn_id: turn_id.to_string(),
            approx_reasoning_tokens,
            approx_output_tokens,
            approx_total_tokens,
        })
    }

    fn remember_silent_reasoning_until(&mut self, now: Instant) {
        let silent_tokens = self.silent_reasoning_tokens_since_last_activity(now);
        self.max_silent_reasoning_tokens = self.max_silent_reasoning_tokens.max(silent_tokens);
    }

    fn silent_reasoning_tokens_since_last_activity(&self, now: Instant) -> i64 {
        let elapsed_ms = now
            .saturating_duration_since(self.last_stream_activity_at)
            .as_millis()
            .min(i64::MAX as u128) as i64;
        elapsed_ms.saturating_mul(APPROX_SILENT_THINKING_TOKENS_PER_SECOND) / 1_000
    }
}

fn approx_tokens_from_bytes(bytes: usize) -> i64 {
    bytes.div_ceil(APPROX_BYTES_PER_TOKEN) as i64
}

#[cfg(test)]
mod tests;
