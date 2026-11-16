//! Server-held conversation continuation.
//!
//! Some backends keep the conversation server-side (an LSD trip, an
//! Anthropic message thread) so a request carries only what is new. The
//! adapter records here what the server already holds, and on the next
//! request sends the remainder when the history still extends it; a
//! rewritten history (compaction, edits) fails the fingerprint and the
//! adapter starts over with the full conversation.
//!
//! Unlike the Responses sticky-routing token, this value changes on every
//! response, so it is a mutable slot rather than a write-once cell.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Continuation {
    /// The server's handle for the stored conversation.
    pub token: String,
    /// How many leading wire messages the server already holds.
    pub prefix_len: usize,
    /// Hash of those messages and of every request field the server pins.
    pub fingerprint: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ContinuationSlot(Arc<Mutex<Option<Continuation>>>);

impl ContinuationSlot {
    pub fn get(&self) -> Option<Continuation> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set(&self, continuation: Continuation) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(continuation);
    }

    pub fn clear(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
mod tests;
