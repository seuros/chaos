use super::*;

#[test]
fn quiescence_requires_a_completed_turn_and_no_pending_work() {
    let mut activity = ProcessActivity::default();
    assert!(disposition(&activity, false).is_none());
    assert!(matches!(
        disposition(&activity, true),
        Some(WaitEvent::Complete)
    ));
    activity.outstanding_tasks = 1;
    assert!(disposition(&activity, true).is_none());
    activity.outstanding_tasks = 0;
    activity.pending_completions = 1;
    assert!(disposition(&activity, true).is_none());
    activity.pending_completions = 0;
    activity.active_turn = true;
    assert!(disposition(&activity, true).is_none());
}

#[test]
fn interrupted_or_blocked_is_not_success() {
    let mut activity = ProcessActivity {
        wake_policy: WakePolicy::Interrupted,
        ..Default::default()
    };
    assert!(matches!(
        disposition(&activity, true),
        Some(WaitEvent::Stopped(_))
    ));
    activity.wake_policy = WakePolicy::Enabled;
    activity.blocked = Some("storage".into());
    assert!(matches!(
        disposition(&activity, false),
        Some(WaitEvent::Stopped(_))
    ));
}
