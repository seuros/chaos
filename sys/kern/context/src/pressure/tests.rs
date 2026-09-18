use super::*;

#[test]
fn window_lifecycle_chains_ids_and_resets_state() {
    let mut window = Window::new();
    assert_eq!(window.window_number(), 0);
    assert_eq!(window.first_window_id(), window.window_id());
    assert_eq!(window.previous_window_id(), None);

    // Estimated baseline yields to the first server observation; further
    // observations and estimates within the window are ignored.
    window.set_estimated_baseline(100);
    assert_eq!(window.baseline(), Some(Baseline::Estimated(100)));
    window.observe_server_baseline(250);
    assert_eq!(window.baseline(), Some(Baseline::ServerObserved(250)));
    window.observe_server_baseline(999);
    window.set_estimated_baseline(1);
    assert_eq!(window.baseline(), Some(Baseline::ServerObserved(250)));

    assert!(window.claim_reminder());
    assert!(!window.claim_reminder());

    let first_id = window.window_id();
    window.advance();
    assert_eq!(window.window_number(), 1);
    assert_eq!(window.previous_window_id(), Some(first_id));
    assert_eq!(window.first_window_id(), first_id);
    assert_ne!(window.window_id(), first_id);
    assert_eq!(window.baseline(), None);
    assert_eq!(window.control(), &Control::Normal);
    assert!(!window.deferral_used());
    assert!(window.claim_reminder());
}

#[test]
fn reconstructed_window_keeps_number_but_uses_fresh_uuid() {
    let window = Window::from_number(7);
    assert_eq!(window.window_number(), 7);
    assert_eq!(window.first_window_id(), window.window_id());
    assert_eq!(window.previous_window_id(), None);
}

#[test]
fn advance_resets_compaction_control() {
    let mut window = Window::new();
    window.defer(Deferral {
        model: "model".to_string(),
        effective_context_window: 100,
        ceiling: 80,
    });
    assert!(window.deferral_used());
    window.advance();
    assert_eq!(window.control(), &Control::Normal);
    assert!(!window.deferral_used());
}
