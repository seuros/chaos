use super::*;
use pretty_assertions::assert_eq;

/// Behavior: for ASCII input we "hold" the first fast char briefly. If no burst follows,
/// that held char should eventually flush as normal typed input (not as a paste).
pub(crate) fn paste_burst_suite() {
    ascii_first_char_is_held_then_flushes_as_typed();
    ascii_two_fast_chars_start_buffer_from_pending_and_flush_as_paste();
    flush_before_modified_input_includes_pending_first_char();
    decide_begin_buffer_only_triggers_for_pastey_prefixes();
    newline_suppression_window_outlives_buffer_flush();
}

fn ascii_first_char_is_held_then_flushes_as_typed() {
    let mut burst = PasteBurst::default();
    let t0 = Instant::now();
    assert!(matches!(
        burst.on_plain_char('a', t0),
        CharDecision::RetainFirstChar
    ));

    let t1 = t0 + PasteBurst::recommended_flush_delay() + Duration::from_millis(1);
    assert!(matches!(burst.flush_if_due(t1), FlushResult::Typed('a')));
    assert!(!burst.is_active());
}

/// Behavior: if two ASCII chars arrive quickly, we should start buffering without ever
/// rendering the first one, then flush the whole buffered payload as a paste.
fn ascii_two_fast_chars_start_buffer_from_pending_and_flush_as_paste() {
    let mut burst = PasteBurst::default();
    let t0 = Instant::now();
    assert!(matches!(
        burst.on_plain_char('a', t0),
        CharDecision::RetainFirstChar
    ));

    let t1 = t0 + Duration::from_millis(1);
    assert!(matches!(
        burst.on_plain_char('b', t1),
        CharDecision::BeginBufferFromPending
    ));
    burst.append_char_to_buffer('b', t1);

    let t2 = t1 + PasteBurst::recommended_active_flush_delay() + Duration::from_millis(1);
    assert!(matches!(
        burst.flush_if_due(t2),
        FlushResult::Paste(ref s) if s == "ab"
    ));
}

/// Behavior: when non-char input is about to be applied, we flush any transient burst state
/// immediately (including a single pending ASCII char) so state doesn't leak across inputs.
fn flush_before_modified_input_includes_pending_first_char() {
    let mut burst = PasteBurst::default();
    let t0 = Instant::now();
    assert!(matches!(
        burst.on_plain_char('a', t0),
        CharDecision::RetainFirstChar
    ));

    assert_eq!(burst.flush_before_modified_input(), Some("a".to_string()));
    assert!(!burst.is_active());
}

/// Behavior: retro-grab buffering is only enabled when the already-inserted prefix looks
/// paste-like (whitespace or "long enough") so short IME bursts don't get misclassified.
fn decide_begin_buffer_only_triggers_for_pastey_prefixes() {
    let mut burst = PasteBurst::default();
    let now = Instant::now();

    assert!(burst.decide_begin_buffer(now, "ab", 2).is_none());
    assert!(!burst.is_active());

    let grab = burst
        .decide_begin_buffer(now, "a b", 2)
        .expect("whitespace should be considered paste-like");
    assert_eq!(grab.start_byte, 1);
    assert_eq!(grab.grabbed, " b");
    assert!(burst.is_active());
}

/// Behavior: after a paste-like burst, we keep an "enter suppression window" alive briefly so
/// a slightly-late Enter still inserts a newline instead of submitting.
fn newline_suppression_window_outlives_buffer_flush() {
    let mut burst = PasteBurst::default();
    let t0 = Instant::now();
    assert!(matches!(
        burst.on_plain_char('a', t0),
        CharDecision::RetainFirstChar
    ));

    let t1 = t0 + Duration::from_millis(1);
    assert!(matches!(
        burst.on_plain_char('b', t1),
        CharDecision::BeginBufferFromPending
    ));
    burst.append_char_to_buffer('b', t1);

    let t2 = t1 + PasteBurst::recommended_active_flush_delay() + Duration::from_millis(1);
    assert!(matches!(burst.flush_if_due(t2), FlushResult::Paste(ref s) if s == "ab"));
    assert!(!burst.is_active());

    assert!(burst.newline_should_insert_instead_of_submit(t2));
    let t3 = t1 + PASTE_ENTER_SUPPRESS_WINDOW + Duration::from_millis(1);
    assert!(!burst.newline_should_insert_instead_of_submit(t3));
}
