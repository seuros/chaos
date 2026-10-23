use super::*;
use chaos_ipc::models::ResponseInputItem;

fn make_input() -> ResponseInputItem {
    ResponseInputItem::Message {
        role: "user".to_string(),
        content: vec![chaos_ipc::models::ContentItem::InputText {
            text: "test".to_string(),
        }],
    }
}

#[test]
fn mailbox_delivery_follows_answer_input_and_tool_boundaries() {
    let mut ts = TurnState::default();
    assert!(ts.accepts_mailbox_delivery());
    assert!(!ts.has_deliverable_input());
    ts.record_answer_emitted();
    assert!(!ts.accepts_mailbox_delivery());
    assert!(!ts.has_deliverable_input());

    ts.push_pending_input(make_input());
    // push_pending_input atomically calls record_steered_input
    assert!(ts.accepts_mailbox_delivery());
    assert!(ts.has_deliverable_input());
    assert_eq!(ts.take_pending_input().len(), 1);
    ts.record_answer_emitted();
    assert!(!ts.accepts_mailbox_delivery());

    ts.record_tool_call_emitted();
    assert!(ts.accepts_mailbox_delivery());
}

#[test]
fn pending_approvals_reject_duplicates_across_kinds_independently() {
    let mut ts = TurnState::default();
    let (tx_a, _rx_a) = oneshot::channel();
    let (tx_b, _rx_b) = oneshot::channel();
    let (tx_c, _rx_c) = oneshot::channel();

    // First exec insert for call_id=foo — accepted.
    std::assert_matches!(
        ts.insert_pending_approval(ApprovalKind::Exec, "foo".into(), tx_a),
        PendingInsert::Inserted
    );
    // Duplicate exec insert for the same call_id — rejected.
    std::assert_matches!(
        ts.insert_pending_approval(ApprovalKind::Exec, "foo".into(), tx_b),
        PendingInsert::Duplicate(_)
    );
    // Patch with the same textual id — accepted (separate namespace).
    std::assert_matches!(
        ts.insert_pending_approval(ApprovalKind::Patch, "foo".into(), tx_c),
        PendingInsert::Inserted
    );
    // Removing by the wrong kind must not find the entry.
    assert!(
        ts.remove_pending_approval(ApprovalKind::Patch, "nope")
            .is_none()
    );
    assert!(
        ts.remove_pending_approval(ApprovalKind::Exec, "foo")
            .is_some()
    );
    assert!(
        ts.remove_pending_approval(ApprovalKind::Patch, "foo")
            .is_some()
    );
}

#[test]
fn stale_defer_does_not_override_steered_input() {
    let mut ts = TurnState::default();
    // User steers input first
    ts.push_pending_input(make_input());
    // Then answer boundary fires — guard: pending_input non-empty, stay CurrentTurn
    ts.record_answer_emitted();
    assert!(ts.accepts_mailbox_delivery());
    assert!(ts.has_deliverable_input());
    let drained = ts.take_pending_input();
    assert_eq!(drained.len(), 1);
}
