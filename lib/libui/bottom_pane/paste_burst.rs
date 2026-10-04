//! Paste-burst detection for terminals without bracketed paste.
//!
//! On some platforms (notably Windows), pastes often arrive as a rapid stream of
//! `KeyCode::Char` and `KeyCode::Enter` key events rather than as a single "paste" event.
//! In that mode, the composer needs to:
//!
//! - Prevent transient UI side effects (e.g. toggles bound to `?`) from triggering on pasted text.
//! - Ensure Enter is treated as a newline *inside the paste*, not as "submit the message".
//! - Avoid flicker caused by inserting a typed prefix and then immediately reclassifying it as
//!   paste once enough chars have arrived.
//!
//! This module provides the `PasteBurst` state machine. `ChatComposer` feeds it only "plain"
//! character events (no Ctrl/Alt) and uses its decisions to either:
//!
//! - briefly hold a first ASCII char (flicker suppression),
//! - buffer a burst as a single pasted string, or
//! - let input flow through as normal typing.
//!
//! For the higher-level view of how `PasteBurst` integrates with `ChatComposer`, see
//! `docs/tui-chat-composer.md`.
//!
//! # Call Pattern
//!
//! `PasteBurst` is a pure state machine: it never mutates the textarea directly. The caller feeds
//! it events and then applies the chosen action:
//!
//! - For each plain `KeyCode::Char`, call [`PasteBurst::on_plain_char`] (ASCII) or
//!   [`PasteBurst::on_plain_char_no_hold`] (non-ASCII/IME).
//! - If the decision indicates buffering, the caller appends to `PasteBurst.buffer` via
//!   [`PasteBurst::append_char_to_buffer`].
//! - On a UI tick, call [`PasteBurst::flush_if_due`]. If it returns [`FlushResult::Typed`], insert
//!   that char as normal typing. If it returns [`FlushResult::Paste`], treat the returned string as
//!   an explicit paste.
//! - Before applying non-char input (arrow keys, Ctrl/Alt modifiers, etc.), use
//!   [`PasteBurst::flush_before_modified_input`] to avoid leaving buffered text "stuck", and then
//!   [`PasteBurst::clear_window_after_non_char`] so subsequent typing does not get grouped into a
//!   previous burst.
//!
//! # Owned Regions
//!
//! Held input, buffer classification/text, character sampling, and Enter suppression are
//! independent owners. Buffer flushes preserve separately held input and the longer suppression
//! deadline. Classification reset preserves residual text; explicit paste clears everything.
//!
//! # Timing Model
//!
//! There are two timeouts:
//!
//! - `PASTE_BURST_CHAR_INTERVAL`: maximum delay between consecutive "plain" chars for them to be
//!   considered part of a single burst. It also bounds how long `pending_first_char` is held.
//! - `PASTE_BURST_ACTIVE_IDLE_TIMEOUT`: once buffering is active, how long to wait after the last
//!   char before flushing the accumulated buffer as a paste.
//!
//! `flush_if_due()` intentionally uses `>` (not `>=`) when comparing elapsed time, so tests and UI
//! ticks should cross the threshold by at least 1ms (see `recommended_flush_delay()`).
//!
//! # Retro Capture Details
//!
//! Retro-capture exists to handle the case where we initially inserted characters as "normal
//! typing", but later decide that the stream is paste-like. When that happens, we retroactively
//! remove a prefix of already-inserted text from the textarea and move it into the burst buffer so
//! the eventual `handle_paste(...)` sees a contiguous pasted string.
//!
//! Retro-capture mostly matters on paths that do *not* hold the first character (non-ASCII/IME
//! input, and retro-grab scenarios). The ASCII path usually prefers
//! `RetainFirstChar -> BeginBufferFromPending`, which avoids needing retro-capture at all.
//!
//! Retro-capture is expressed in terms of characters, not bytes:
//!
//! - `CharDecision::BeginBuffer { retro_chars }` uses `retro_chars` as a character count.
//! - `decide_begin_buffer(now, before_cursor, retro_chars)` turns that into a UTF-8 byte range by
//!   calling `retro_start_index()`.
//! - `RetroGrab.start_byte` is a byte index into the `before_cursor` slice; callers must clamp the
//!   cursor to a char boundary before slicing so `start_byte..cursor` is always valid UTF-8.
//!
//! # Clearing vs Flushing
//!
//! There are two ways callers end burst handling, and they are not interchangeable:
//!
//! - `flush_before_modified_input()` returns the buffered text (and/or a pending first ASCII char)
//!   so the caller can apply it through the normal paste path before handling an unrelated input.
//! - `clear_window_after_non_char()` clears the *classification window* so subsequent typing does
//!   not get grouped into the previous burst. It assumes the caller has already flushed any buffer
//!   because it clears `last_plain_char_time`, which means `flush_if_due()` will not flush a
//!   non-empty buffer until another plain char updates the timestamp.
//!
//! # States (Conceptually)
//!
//! - **Idle**: no buffered text, no pending char.
//! - **Pending first char**: `pending_first_char` holds one ASCII char for up to
//!   `PASTE_BURST_CHAR_INTERVAL` while we wait to see if a burst follows.
//! - **Active buffer**: the Buffering classification holds paste-like content until it times out.
//! - **Enter suppress window**: `burst_window_until` keeps Enter treated as newline briefly after
//!   burst activity so multiline pastes stay grouped.
//!
//! # ASCII vs Non-ASCII
//!
//! - [`PasteBurst::on_plain_char`] may return [`CharDecision::RetainFirstChar`] to hold the first
//!   ASCII char and avoid flicker.
//! - [`PasteBurst::on_plain_char_no_hold`] never holds (used for IME/non-ASCII paths), since
//!   holding a non-ASCII character can feel like dropped input.
//!
//! # Contract With `ChatComposer`
//!
//! `PasteBurst` does not mutate the UI text buffer on its own. The caller (`ChatComposer`) must
//! interpret decisions and apply the corresponding UI edits:
//!
//! - For each plain ASCII `KeyCode::Char`, call [`PasteBurst::on_plain_char`].
//!   - [`CharDecision::RetainFirstChar`]: do **not** insert the char into the textarea yet.
//!   - [`CharDecision::BeginBufferFromPending`]: call [`PasteBurst::append_char_to_buffer`] for the
//!     current char (the previously-held char is already in the burst buffer).
//!   - [`CharDecision::BeginBuffer { retro_chars }`]: consider retro-capturing the already-inserted
//!     prefix by calling [`PasteBurst::decide_begin_buffer`]. If it returns `Some`, remove the
//!     returned `start_byte..cursor` range from the textarea and then call
//!     [`PasteBurst::append_char_to_buffer`] for the current char. If it returns `None`, fall back
//!     to normal insertion.
//!   - [`CharDecision::BufferAppend`]: call [`PasteBurst::append_char_to_buffer`].
//!
//! - For each plain non-ASCII `KeyCode::Char`, call [`PasteBurst::on_plain_char_no_hold`] and then:
//!   - If it returns `Some(CharDecision::BufferAppend)`, call
//!     [`PasteBurst::append_char_to_buffer`].
//!   - If it returns `Some(CharDecision::BeginBuffer { retro_chars })`, call
//!     [`PasteBurst::decide_begin_buffer`] as above (and if buffering starts, remove the grabbed
//!     prefix from the textarea and then append the current char to the buffer).
//!   - If it returns `None`, insert normally.
//!
//! - Before applying non-char input (or any input that should not join a burst), call
//!   [`PasteBurst::flush_before_modified_input`] and pass the returned string (if any) through the
//!   normal paste path.
//!
//! - Periodically (e.g. on a UI tick), call [`PasteBurst::flush_if_due`].
//!   - [`FlushResult::Typed`]: insert that single char as normal typing.
//!   - [`FlushResult::Paste`]: treat the returned string as an explicit paste.
//!
//! - When a non-plain key is pressed (Ctrl/Alt-modified input, arrows, etc.), callers should use
//!   [`PasteBurst::clear_window_after_non_char`] to prevent the next keystroke from being
//!   incorrectly grouped into a previous burst.

use std::time::Duration;
use std::time::Instant;

mod lifecycle;
use lifecycle::Lifecycle;

// Heuristic thresholds for detecting paste-like input bursts.
// Detect quickly to avoid showing typed prefix before paste is recognized
const PASTE_BURST_MIN_CHARS: u16 = 3;
const PASTE_ENTER_SUPPRESS_WINDOW: Duration = Duration::from_millis(120);

// Maximum delay between consecutive chars to be considered part of a paste burst.
const PASTE_BURST_CHAR_INTERVAL: Duration = Duration::from_millis(8);

// Idle timeout before flushing buffered paste content.
const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(8);

#[derive(Default)]
pub struct PasteBurst {
    lifecycle: Lifecycle,
}

#[derive(Debug)]
pub enum CharDecision {
    /// Start buffering and retroactively capture some already-inserted chars.
    BeginBuffer { retro_chars: u16 },
    /// We are currently buffering; append the current char into the buffer.
    BufferAppend,
    /// Do not insert/render this char yet; temporarily save the first fast
    /// char while we wait to see if a paste-like burst follows.
    RetainFirstChar,
    /// Begin buffering using the previously saved first char (no retro grab needed).
    BeginBufferFromPending,
}

pub struct RetroGrab {
    pub start_byte: usize,
    pub grabbed: String,
}

#[derive(Debug)]
pub enum FlushResult {
    Paste(String),
    Typed(char),
    None,
}

impl PasteBurst {
    /// Recommended delay to wait between simulated keypresses (or before
    /// scheduling a UI tick) so that a pending fast keystroke is flushed
    /// out of the burst detector as normal typed input.
    ///
    /// Primarily used by tests and by the TUI to reliably cross the
    /// paste-burst timing threshold.
    pub fn recommended_flush_delay() -> Duration {
        PASTE_BURST_CHAR_INTERVAL + Duration::from_millis(1)
    }

    #[cfg(test)]
    pub fn recommended_active_flush_delay() -> Duration {
        PASTE_BURST_ACTIVE_IDLE_TIMEOUT + Duration::from_millis(1)
    }

    /// Entry point: decide how to treat a plain char with current timing.
    pub fn on_plain_char(&mut self, ch: char, now: Instant) -> CharDecision {
        self.lifecycle.note_plain_char(now);

        if self.lifecycle.buffering() {
            self.lifecycle.extend_window(now);
            return CharDecision::BufferAppend;
        }

        // If we already held a first char and receive a second fast char,
        // start buffering without retro-grabbing (we never rendered the first).
        if let Some((held, held_at)) = self.lifecycle.held()
            && now.duration_since(held_at) <= PASTE_BURST_CHAR_INTERVAL
        {
            self.lifecycle.take_held();
            self.lifecycle.begin();
            self.lifecycle.buffer_mut().push(held);
            self.lifecycle.extend_window(now);
            return CharDecision::BeginBufferFromPending;
        }

        if self.lifecycle.consecutive_chars() >= PASTE_BURST_MIN_CHARS {
            return CharDecision::BeginBuffer {
                retro_chars: self.lifecycle.consecutive_chars().saturating_sub(1),
            };
        }

        // Save the first fast char very briefly to see if a burst follows.
        self.lifecycle.hold(ch, now);
        CharDecision::RetainFirstChar
    }

    /// Like on_plain_char(), but never holds the first char.
    ///
    /// Used for non-ASCII input paths (e.g., IMEs) where holding a character can
    /// feel like dropped input, while still allowing burst-based paste detection.
    ///
    /// Note: This method will only ever return BufferAppend or BeginBuffer.
    pub fn on_plain_char_no_hold(&mut self, now: Instant) -> Option<CharDecision> {
        self.lifecycle.note_plain_char(now);

        if self.lifecycle.buffering() {
            self.lifecycle.extend_window(now);
            return Some(CharDecision::BufferAppend);
        }

        if self.lifecycle.consecutive_chars() >= PASTE_BURST_MIN_CHARS {
            return Some(CharDecision::BeginBuffer {
                retro_chars: self.lifecycle.consecutive_chars().saturating_sub(1),
            });
        }

        None
    }

    /// Flushes any buffered burst if the inter-key timeout has elapsed.
    ///
    /// Returns:
    ///
    /// - [`FlushResult::Paste`] when a paste burst was active and buffered text is emitted as one
    ///   pasted string.
    /// - [`FlushResult::Typed`] when a single fast first ASCII char was being held (flicker
    ///   suppression) and no burst followed before the timeout elapsed.
    /// - [`FlushResult::None`] when the timeout has not elapsed, or there is nothing to flush.
    pub fn flush_if_due(&mut self, now: Instant) -> FlushResult {
        self.lifecycle.expire_window(now);
        let timeout = if self.is_active_internal() {
            PASTE_BURST_ACTIVE_IDLE_TIMEOUT
        } else {
            PASTE_BURST_CHAR_INTERVAL
        };
        let timed_out = self
            .lifecycle
            .last_plain_char_time()
            .is_some_and(|t| now.duration_since(t) > timeout);
        if timed_out && self.is_active_internal() {
            FlushResult::Paste(self.lifecycle.flush_buffer())
        } else if timed_out {
            // If we were saving a single fast char and no burst followed,
            // flush it as normal typed input.
            if let Some(ch) = self.lifecycle.take_held() {
                FlushResult::Typed(ch)
            } else {
                FlushResult::None
            }
        } else {
            FlushResult::None
        }
    }

    /// While bursting: accumulate a newline into the buffer instead of
    /// submitting the textarea.
    ///
    /// Returns true if a newline was appended (we are in a burst context),
    /// false otherwise.
    pub fn append_newline_if_active(&mut self, now: Instant) -> bool {
        if self.is_active() {
            self.lifecycle.buffer_mut().push('\n');
            self.lifecycle.extend_window(now);
            true
        } else {
            false
        }
    }

    /// Decide if Enter should insert a newline (burst context) vs submit.
    pub fn newline_should_insert_instead_of_submit(&self, now: Instant) -> bool {
        self.is_active() || self.lifecycle.suppresses_enter(now)
    }

    /// Keep the burst window alive.
    pub fn extend_window(&mut self, now: Instant) {
        self.lifecycle.extend_window(now);
    }

    /// Begin buffering with retroactively grabbed text.
    pub fn begin_with_retro_grabbed(&mut self, grabbed: String, now: Instant) {
        if !grabbed.is_empty() {
            self.lifecycle.buffer_mut().push_str(&grabbed);
        }
        self.lifecycle.begin();
        self.lifecycle.extend_window(now);
    }

    /// Append a char into the burst buffer.
    pub fn append_char_to_buffer(&mut self, ch: char, now: Instant) {
        self.lifecycle.buffer_mut().push(ch);
        self.lifecycle.extend_window(now);
    }

    /// Try to append a char into the burst buffer only if a burst is already active.
    ///
    /// Returns true when the char was captured into the existing burst, false otherwise.
    pub fn try_append_char_if_active(&mut self, ch: char, now: Instant) -> bool {
        if self.lifecycle.buffering() || !self.lifecycle.buffer().is_empty() {
            self.append_char_to_buffer(ch, now);
            true
        } else {
            false
        }
    }

    /// Decide whether to begin buffering by retroactively capturing recent
    /// chars from the slice before the cursor.
    ///
    /// Heuristic: if the retro-grabbed slice contains any whitespace or is
    /// sufficiently long (>= 16 characters), treat it as paste-like to avoid
    /// rendering the typed prefix momentarily before the paste is recognized.
    /// This favors responsiveness and prevents flicker for typical pastes
    /// (URLs, file paths, multiline text) while not triggering on short words.
    ///
    /// Returns Some(RetroGrab) with the start byte and grabbed text when we
    /// decide to buffer retroactively; otherwise None.
    pub fn decide_begin_buffer(
        &mut self,
        now: Instant,
        before: &str,
        retro_chars: usize,
    ) -> Option<RetroGrab> {
        let start_byte = retro_start_index(before, retro_chars);
        let grabbed = before[start_byte..].to_string();
        let looks_pastey =
            grabbed.chars().any(char::is_whitespace) || grabbed.chars().count() >= 16;
        if looks_pastey {
            // Note: caller is responsible for removing this slice from UI text.
            self.begin_with_retro_grabbed(grabbed.clone(), now);
            Some(RetroGrab {
                start_byte,
                grabbed,
            })
        } else {
            None
        }
    }

    /// Before applying modified/non-char input: flush buffered burst immediately.
    pub fn flush_before_modified_input(&mut self) -> Option<String> {
        if !self.is_active() {
            return None;
        }
        let mut out = self.lifecycle.flush_buffer();
        if let Some(ch) = self.lifecycle.take_held() {
            out.push(ch);
        }
        Some(out)
    }

    /// Clear only the timing window and any pending first-char.
    ///
    /// Does not emit or clear the buffered text itself; callers should have
    /// already flushed (if needed) via one of the flush methods above.
    pub fn clear_window_after_non_char(&mut self) {
        self.lifecycle.clear_window();
    }

    /// Returns true if we are in any paste-burst related transient state
    /// (actively buffering, have a non-empty buffer, or have saved the first
    /// fast char while waiting for a potential burst).
    pub fn is_active(&self) -> bool {
        self.is_active_internal() || self.lifecycle.held().is_some()
    }

    fn is_active_internal(&self) -> bool {
        self.lifecycle.buffering() || !self.lifecycle.buffer().is_empty()
    }

    pub fn clear_after_explicit_paste(&mut self) {
        self.lifecycle.clear_window();
        self.lifecycle.buffer_mut().clear();
    }
}

pub fn retro_start_index(before: &str, retro_chars: usize) -> usize {
    if retro_chars == 0 {
        return before.len();
    }
    before
        .char_indices()
        .rev()
        .nth(retro_chars.saturating_sub(1))
        .map(|(idx, _)| idx)
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) mod tests;
