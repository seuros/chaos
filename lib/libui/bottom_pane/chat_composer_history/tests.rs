use super::*;
use crate::test_support::make_app_event_sender_with_rx;
use chaos_ipc::protocol::Op;
use pretty_assertions::assert_eq;

pub(crate) fn chat_composer_history_suite() {
    duplicate_submissions_are_not_recorded();
    navigation_with_async_fetch();
    reset_navigation_resets_cursor();
    should_handle_navigation_when_cursor_is_at_line_boundaries();
}

fn duplicate_submissions_are_not_recorded() {
    let mut history = ChatComposerHistory::new();

    // Empty submissions are ignored.
    history.record_local_submission(HistoryEntry::new(String::new()));
    assert_eq!(history.local_history.len(), 0);

    // First entry is recorded.
    history.record_local_submission(HistoryEntry::new("hello".to_string()));
    assert_eq!(history.local_history.len(), 1);
    assert_eq!(
        history.local_history.last().unwrap(),
        &HistoryEntry::new("hello".to_string())
    );

    // Identical consecutive entry is skipped.
    history.record_local_submission(HistoryEntry::new("hello".to_string()));
    assert_eq!(history.local_history.len(), 1);

    // Different entry is recorded.
    history.record_local_submission(HistoryEntry::new("world".to_string()));
    assert_eq!(history.local_history.len(), 2);
    assert_eq!(
        history.local_history.last().unwrap(),
        &HistoryEntry::new("world".to_string())
    );
}

fn navigation_with_async_fetch() {
    let (tx, mut rx) = make_app_event_sender_with_rx();

    let mut history = ChatComposerHistory::new();
    // Pretend there are 3 persistent entries.
    history.set_metadata(1, 3);

    // First Up should request offset 2 (latest) and await async data.
    assert!(history.should_handle_navigation("", 0));
    assert!(history.navigate_up(&tx).is_none()); // don't replace the text yet

    // Verify that an AppEvent::ChaosOp with the correct GetHistoryEntryRequest was sent.
    let event = rx.try_recv().expect("expected AppEvent to be sent");
    let AppEvent::ChaosOp(history_request1) = event else {
        panic!("unexpected event variant");
    };
    assert_eq!(
        Op::GetHistoryEntryRequest {
            log_id: 1,
            offset: 2
        },
        history_request1
    );

    // Inject the async response.
    assert_eq!(
        Some(HistoryEntry::new("latest".to_string())),
        history.on_entry_response(1, 2, Some("latest".into()))
    );

    // Next Up should move to offset 1.
    assert!(history.navigate_up(&tx).is_none()); // don't replace the text yet

    // Verify second ChaosOp event for offset 1.
    let event2 = rx.try_recv().expect("expected second event");
    let AppEvent::ChaosOp(history_request_2) = event2 else {
        panic!("unexpected event variant");
    };
    assert_eq!(
        Op::GetHistoryEntryRequest {
            log_id: 1,
            offset: 1
        },
        history_request_2
    );

    assert_eq!(
        Some(HistoryEntry::new("older".to_string())),
        history.on_entry_response(1, 1, Some("older".into()))
    );
}

fn reset_navigation_resets_cursor() {
    let tx = crate::test_support::make_app_event_sender();

    let mut history = ChatComposerHistory::new();
    history.set_metadata(1, 3);
    history
        .fetched_history
        .insert(1, HistoryEntry::new("command2".to_string()));
    history
        .fetched_history
        .insert(2, HistoryEntry::new("command3".to_string()));

    assert_eq!(
        Some(HistoryEntry::new("command3".to_string())),
        history.navigate_up(&tx)
    );
    assert_eq!(
        Some(HistoryEntry::new("command2".to_string())),
        history.navigate_up(&tx)
    );

    history.reset_navigation();
    assert!(history.history_cursor.is_none());
    assert!(history.last_history_text.is_none());

    assert_eq!(
        Some(HistoryEntry::new("command3".to_string())),
        history.navigate_up(&tx)
    );
}

fn should_handle_navigation_when_cursor_is_at_line_boundaries() {
    let mut history = ChatComposerHistory::new();
    history.record_local_submission(HistoryEntry::new("hello".to_string()));
    history.last_history_text = Some("hello".to_string());

    assert!(history.should_handle_navigation("hello", 0));
    assert!(history.should_handle_navigation("hello", "hello".len()));
    assert!(!history.should_handle_navigation("hello", 1));
    assert!(!history.should_handle_navigation("other", 0));
}
