use super::*;
use pretty_assertions::assert_eq;

pub(crate) fn frame_rate_limiter_suite() {
    default_does_not_clamp();
    clamps_to_min_interval_since_last_emit();
}

fn default_does_not_clamp() {
    let t0 = Instant::now();
    let limiter = FrameRateLimiter::default();
    assert_eq!(limiter.clamp_deadline(t0), t0);
}

fn clamps_to_min_interval_since_last_emit() {
    let t0 = Instant::now();
    let mut limiter = FrameRateLimiter::default();

    assert_eq!(limiter.clamp_deadline(t0), t0);
    limiter.mark_emitted(t0);

    let too_soon = t0 + Duration::from_millis(1);
    assert_eq!(limiter.clamp_deadline(too_soon), t0 + MIN_FRAME_INTERVAL);
}
