use super::*;

#[test]
fn resetting_selection_does_not_end_preview_or_acknowledge_rollback() {
    let mut state = BacktrackState::default();
    state.prime();
    state.preview();
    assert!(state.primed());
    assert!(state.preview_active());
    let mut pending = PendingBacktrackRollback {
        selection: super::super::BacktrackSelection {
            nth_user_message: 0,
            prefill: "original draft".into(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        },
        process_id: None,
    };
    assert!(state.request_rollback(pending.clone()));
    state.unprime();
    assert!(!state.primed());
    assert!(state.preview_active());
    state.close_preview();
    assert!(!state.preview_active());
    assert!(state.rollback_pending());
    pending.selection.prefill = "replacement".into();
    assert!(!state.request_rollback(pending));
    assert_eq!(
        state.take_rollback().unwrap().selection.prefill,
        "original draft"
    );
    assert!(state.take_rollback().is_none());
    assert!(!state.rollback_pending());
    state.preview();
    assert!(state.preview_active());
    assert!(!state.primed());
    state.prime();
    state.close_preview();
    assert!(state.primed());
    assert!(!state.preview_active());
}
