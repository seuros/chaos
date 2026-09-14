//! Frame draw scheduling utilities for the TUI.
//!
//! This module exposes [`FrameRequester`], a lightweight handle that widgets and
//! background tasks can clone to request future redraws of the TUI.
//!
//! Internally it spawns a [`FrameScheduler`] task that coalesces many requests
//! into a single notification on a broadcast channel used by the main TUI event
//! loop. This keeps animations and status updates smooth without redrawing more
//! often than necessary.
//!
//! This follows the actor-style design from
//! [“Actors with Tokio”](https://ryhl.io/blog/actors-with-tokio/), with a
//! dedicated scheduler task and lightweight request handles.

use std::time::Duration;
use std::time::Instant;

use tokio::sync::broadcast;
use tokio::sync::mpsc;

use super::frame_rate_limiter::FrameRateLimiter;

/// A requester for scheduling future frame draws on the TUI event loop.
///
/// This is the handler side of an actor/handler pair with `FrameScheduler`, which coalesces
/// multiple frame requests into a single draw operation.
///
/// Clones of this type can be freely shared across tasks to make it possible to trigger frame draws
/// from anywhere in the TUI code.
#[derive(Clone, Debug)]
pub struct FrameRequester {
    frame_schedule_tx: mpsc::UnboundedSender<Instant>,
}

impl FrameRequester {
    /// Create a new FrameRequester and spawn its associated FrameScheduler task.
    ///
    /// The provided `draw_tx` is used to notify the TUI event loop of scheduled draws.
    pub fn new(draw_tx: broadcast::Sender<()>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let scheduler = FrameScheduler::new(rx, draw_tx);
        tokio::spawn(scheduler.run());
        Self {
            frame_schedule_tx: tx,
        }
    }

    /// Schedule a frame draw as soon as possible.
    pub fn schedule_frame(&self) {
        let _ = self.frame_schedule_tx.send(Instant::now());
    }

    /// Schedule a frame draw to occur after the specified duration.
    pub fn schedule_frame_in(&self, dur: Duration) {
        let _ = self.frame_schedule_tx.send(Instant::now() + dur);
    }
}

impl FrameRequester {
    /// Create a no-op frame requester for tests.
    pub fn test_dummy() -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        FrameRequester {
            frame_schedule_tx: tx,
        }
    }
}

/// A scheduler for coalescing frame draw requests and notifying the TUI event loop.
///
/// This type is internal to `FrameRequester` and is spawned as a task to handle scheduling logic.
///
/// To avoid wasted redraw work, draw notifications are clamped to a maximum of 120 FPS (see
/// [`FrameRateLimiter`]).
struct FrameScheduler {
    receiver: mpsc::UnboundedReceiver<Instant>,
    draw_tx: broadcast::Sender<()>,
    rate_limiter: FrameRateLimiter,
}

impl FrameScheduler {
    /// Create a new FrameScheduler with the provided receiver and draw notification sender.
    fn new(receiver: mpsc::UnboundedReceiver<Instant>, draw_tx: broadcast::Sender<()>) -> Self {
        Self {
            receiver,
            draw_tx,
            rate_limiter: FrameRateLimiter::default(),
        }
    }

    /// Run the scheduling loop, coalescing frame requests and notifying the TUI event loop.
    ///
    /// This method runs indefinitely until all senders are dropped. A single draw notification
    /// is sent for multiple requests scheduled before the next draw deadline.
    async fn run(mut self) {
        const ONE_YEAR: Duration = Duration::from_secs(60 * 60 * 24 * 365);
        let mut next_deadline: Option<Instant> = None;
        loop {
            let target = next_deadline.unwrap_or_else(|| Instant::now() + ONE_YEAR);
            let deadline = tokio::time::sleep_until(target.into());
            tokio::pin!(deadline);

            tokio::select! {
                draw_at = self.receiver.recv() => {
                    let Some(draw_at) = draw_at else {
                        // All senders dropped; exit the scheduler.
                        break
                    };
                    let draw_at = self.rate_limiter.clamp_deadline(draw_at);
                    next_deadline = Some(next_deadline.map_or(draw_at, |cur| cur.min(draw_at)));

                    // Do not send a draw immediately here. By continuing the loop,
                    // we recompute the sleep target so the draw fires once via the
                    // sleep branch, coalescing multiple requests into a single draw.
                    continue;
                }
                _ = &mut deadline => {
                    if next_deadline.is_some() {
                        next_deadline = None;
                        self.rate_limiter.mark_emitted(target);
                        let _ = self.draw_tx.send(());
                    }
                }
            }
        }
    }
}
#[cfg(test)]
pub(crate) mod tests;
