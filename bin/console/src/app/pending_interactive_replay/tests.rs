use super::super::ProcessEventStore;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::TurnAbortReason;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::path::PathBuf;

fn request_user_input_event(event_id: &str, call_id: &str, turn_id: &str) -> Event {
    Event {
        id: event_id.to_string(),
        msg: EventMsg::RequestUserInput(chaos_ipc::request_user_input::RequestUserInputEvent {
            call_id: call_id.to_string(),
            turn_id: turn_id.to_string(),
            questions: Vec::new(),
        }),
    }
}

fn exec_approval_event(
    event_id: &str,
    call_id: &str,
    approval_id: Option<&str>,
    turn_id: &str,
) -> Event {
    Event {
        id: event_id.to_string(),
        msg: EventMsg::ExecApprovalRequest(chaos_ipc::protocol::ExecApprovalRequestEvent {
            call_id: call_id.to_string(),
            approval_id: approval_id.map(str::to_string),
            turn_id: turn_id.to_string(),
            command: vec!["echo".to_string(), "hi".to_string()],
            cwd: PathBuf::from("/tmp"),
            reason: None,
            network_approval_context: None,
            proposed_execpolicy_amendment: None,
            proposed_network_policy_amendments: None,
            additional_permissions: None,
            available_decisions: None,
            parsed_cmd: Vec::new(),
        }),
    }
}

fn patch_approval_event(event_id: &str, call_id: &str, turn_id: &str) -> Event {
    Event {
        id: event_id.to_string(),
        msg: EventMsg::ApplyPatchApprovalRequest(
            chaos_ipc::protocol::ApplyPatchApprovalRequestEvent {
                call_id: call_id.to_string(),
                turn_id: turn_id.to_string(),
                changes: HashMap::new(),
                reason: None,
                grant_root: None,
            },
        ),
    }
}

fn elicitation_event(
    event_id: &str,
    server_name: &str,
    request_id: chaos_ipc::mcp::RequestId,
) -> Event {
    Event {
        id: event_id.to_string(),
        msg: EventMsg::ElicitationRequest(chaos_ipc::approvals::ElicitationRequestEvent {
            turn_id: Some("turn-1".to_string()),
            server_name: server_name.to_string(),
            id: request_id,
            request: chaos_ipc::approvals::ElicitationRequest::Form {
                meta: None,
                message: "Please confirm".to_string(),
                requested_schema: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
        }),
    }
}

fn user_input_answer(turn_id: &str) -> Op {
    Op::UserInputAnswer {
        id: turn_id.to_string(),
        response: chaos_ipc::request_user_input::RequestUserInputResponse {
            answers: HashMap::new(),
        },
    }
}

fn snapshot_request_user_input_call_ids(store: &ProcessEventStore) -> Vec<String> {
    store
        .snapshot()
        .events
        .iter()
        .filter_map(|event| match &event.msg {
            EventMsg::RequestUserInput(ev) => Some(ev.call_id.clone()),
            _ => None,
        })
        .collect()
}

pub(crate) fn pending_interactive_replay_suite() {
    request_user_input_snapshot_replay_tracks_pending_prompts_fifo_by_turn();
    resolved_process_approvals_and_elicitations_are_filtered_from_snapshots();
    turn_abort_filters_pending_turn_scoped_approval_prompts();
}

fn request_user_input_snapshot_replay_tracks_pending_prompts_fifo_by_turn() {
    let mut store = ProcessEventStore::new(8);
    store.push_event(request_user_input_event("ev-1", "call-1", "turn-1"));
    assert_eq!(snapshot_request_user_input_call_ids(&store), vec!["call-1"]);

    store.note_outbound_op(&user_input_answer("turn-1"));
    assert!(
        store.snapshot().events.is_empty(),
        "resolved request_user_input prompt should not replay on thread switch"
    );

    let mut store = ProcessEventStore::new(8);
    store.push_event(request_user_input_event("ev-1", "call-1", "turn-1"));
    store.note_outbound_op(&user_input_answer("turn-1"));
    store.push_event(request_user_input_event("ev-2", "call-2", "turn-1"));
    assert_eq!(snapshot_request_user_input_call_ids(&store), vec!["call-2"]);

    let mut store = ProcessEventStore::new(8);
    store.push_event(request_user_input_event("ev-1", "call-1", "turn-1"));
    store.push_event(request_user_input_event("ev-2", "call-2", "turn-1"));
    store.note_outbound_op(&user_input_answer("turn-1"));
    assert_eq!(snapshot_request_user_input_call_ids(&store), vec!["call-2"]);
}

fn resolved_process_approvals_and_elicitations_are_filtered_from_snapshots() {
    let mut store = ProcessEventStore::new(8);
    assert_eq!(store.has_pending_process_approvals(), false);
    store.push_event(exec_approval_event(
        "ev-1",
        "call-1",
        Some("approval-1"),
        "turn-1",
    ));
    assert_eq!(store.has_pending_process_approvals(), true);
    store.note_outbound_op(&Op::ExecApproval {
        id: "approval-1".to_string(),
        turn_id: Some("turn-1".to_string()),
        decision: chaos_ipc::protocol::ReviewDecision::Approved,
    });
    assert_eq!(store.has_pending_process_approvals(), false);
    assert!(
        store.snapshot().events.is_empty(),
        "resolved exec approval prompt should not replay on thread switch"
    );

    let mut store = ProcessEventStore::new(8);
    store.push_event(patch_approval_event("ev-1", "call-1", "turn-1"));
    store.note_outbound_op(&Op::PatchApproval {
        id: "call-1".to_string(),
        decision: chaos_ipc::protocol::ReviewDecision::Approved,
    });
    assert!(
        store.snapshot().events.is_empty(),
        "resolved patch approval prompt should not replay on thread switch"
    );

    let mut store = ProcessEventStore::new(8);
    let request_id = chaos_ipc::mcp::RequestId::String("request-1".to_string());
    store.push_event(elicitation_event("ev-1", "server-1", request_id.clone()));
    store.note_outbound_op(&Op::ResolveElicitation {
        server_name: "server-1".to_string(),
        request_id,
        decision: chaos_ipc::approvals::ElicitationAction::Accept,
        content: None,
        meta: None,
    });
    assert!(
        store.snapshot().events.is_empty(),
        "resolved elicitation prompt should not replay on thread switch"
    );

    let mut store = ProcessEventStore::new(8);
    store.push_event(request_user_input_event("ev-1", "call-1", "turn-1"));
    assert_eq!(store.has_pending_process_approvals(), false);
}

fn turn_abort_filters_pending_turn_scoped_approval_prompts() {
    let mut store = ProcessEventStore::new(8);
    store.push_event(exec_approval_event(
        "ev-1",
        "exec-call-1",
        Some("approval-1"),
        "turn-1",
    ));
    store.push_event(patch_approval_event("ev-2", "patch-call-1", "turn-1"));
    store.push_event(Event {
        id: "ev-3".to_string(),
        msg: EventMsg::TurnAborted(chaos_ipc::protocol::TurnAbortedEvent {
            turn_id: Some("turn-1".to_string()),
            reason: TurnAbortReason::Replaced,
        }),
    });

    let snapshot = store.snapshot();
    assert!(snapshot.events.iter().all(|event| {
        !matches!(
            &event.msg,
            EventMsg::ExecApprovalRequest(_) | EventMsg::ApplyPatchApprovalRequest(_)
        )
    }));
}
