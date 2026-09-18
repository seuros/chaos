use super::*;

#[test]
fn emits_observed_output_progress() {
    let start = Instant::now();
    let mut progress = TurnProgressTracker::new_at(start);

    progress.observe_output_delta_at("hello", start);
    let event = progress
        .event_if_due_at("turn-1", start)
        .expect("first observed progress should emit immediately");

    assert_eq!(event.turn_id, "turn-1");
    assert_eq!(event.approx_output_tokens, 2);
    assert_eq!(event.approx_total_tokens, 2);
}

#[test]
fn emits_silent_reasoning_progress_without_provider_usage() {
    let start = Instant::now();
    let mut progress = TurnProgressTracker::new_at(start);

    let event = progress
        .event_if_due_at("turn-1", start + Duration::from_secs(2))
        .expect("silent progress should emit");

    assert_eq!(event.approx_reasoning_tokens, 50);
    assert_eq!(event.approx_output_tokens, 0);
    assert_eq!(event.approx_total_tokens, 50);
}

#[test]
fn silent_reasoning_does_not_grow_from_turn_start_after_stream_activity() {
    let start = Instant::now();
    let mut progress = TurnProgressTracker::new_at(start);

    let first = start + Duration::from_secs(2);
    assert_eq!(
        progress
            .event_if_due_at("turn-1", first)
            .expect("silent progress")
            .approx_reasoning_tokens,
        50
    );

    progress.observe_output_delta_at("abcd", first);
    let after_activity = first + Duration::from_millis(500);
    let event = progress
        .event_if_due_at("turn-1", after_activity)
        .expect("output should increase total without counting all turn time as silent");

    assert_eq!(event.approx_reasoning_tokens, 50);
    assert_eq!(event.approx_output_tokens, 1);
    assert_eq!(event.approx_total_tokens, 51);
}

#[test]
fn throttles_duplicate_progress() {
    let start = Instant::now();
    let mut progress = TurnProgressTracker::new_at(start);

    progress.observe_output_delta_at("abcd", start);
    assert!(progress.event_if_due_at("turn-1", start).is_some());
    assert!(
        progress
            .event_if_due_at("turn-1", start + Duration::from_millis(100))
            .is_none()
    );
}
