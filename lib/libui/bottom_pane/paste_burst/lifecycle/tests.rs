use super::*;

#[test]
fn flushing_buffer_preserves_a_separately_held_character() {
    let now = Instant::now();
    let mut burst = super::super::PasteBurst::default();
    burst.on_plain_char('x', now);
    burst.begin_with_retro_grabbed("前 ".into(), now);
    assert!(matches!(
        burst.flush_if_due(now + super::super::PasteBurst::recommended_active_flush_delay()),
        super::super::FlushResult::Paste(text) if text == "前 "
    ));
    assert!(matches!(
        burst.flush_if_due(now + super::super::PasteBurst::recommended_active_flush_delay()),
        super::super::FlushResult::Typed('x')
    ));
    burst.clear_window_after_non_char();
    assert!(!burst.is_active());
}
