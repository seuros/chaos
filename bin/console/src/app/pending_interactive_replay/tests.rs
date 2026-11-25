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
        call_id: None,
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

fn snapshot_ids(store: &ProcessEventStore) -> Vec<String> {
    store
        .snapshot()
        .events
        .into_iter()
        .map(|ev| ev.id)
        .collect()
}

#[test]
fn request_user_input_snapshot_replay_tracks_pending_prompts_fifo_by_turn() {
    let mut store = ProcessEventStore::new(8);
    store.push_event(request_user_input_event("ev-1", "call-1", "turn-1"));
    assert_eq!(snapshot_request_user_input_call_ids(&store), vec!["call-1"]);

    store.note_outbound_op(&user_input_answer("turn-1"));
    assert!(
        store.snapshot().events.is_empty(),
        "resolved request_user_input prompt should not replay on thread switch"
    );

    store.push_event(request_user_input_event("other", "other-call", "turn-2"));
    store.push_event(request_user_input_event("ev-2", "call-2", "turn-1"));
    assert_eq!(
        snapshot_request_user_input_call_ids(&store),
        ["other-call", "call-2"]
    );
    store.push_event(request_user_input_event("ev-3", "call-3", "turn-1"));
    store.note_outbound_op(&user_input_answer("turn-1"));
    assert_eq!(
        snapshot_request_user_input_call_ids(&store),
        ["other-call", "call-3"]
    );
    assert!(!store.has_pending_process_approvals());
}

#[test]
fn resolved_process_approvals_and_elicitations_are_filtered_from_snapshots() {
    let mut store = ProcessEventStore::new(8);
    assert!(!store.has_pending_process_approvals());
    store.push_event(exec_approval_event(
        "exec",
        "call-1",
        Some("approval-1"),
        "turn-1",
    ));
    assert!(store.has_pending_process_approvals());
    assert_eq!(snapshot_ids(&store), ["exec"]);
    store.note_outbound_op(&Op::ExecApproval {
        id: "approval-1".to_string(),
        turn_id: Some("turn-1".to_string()),
        decision: chaos_ipc::protocol::ReviewDecision::Approved,
    });
    assert!(!store.has_pending_process_approvals());
    assert!(snapshot_ids(&store).is_empty());

    store.push_event(patch_approval_event("patch", "patch-call", "turn-1"));
    let request_id = chaos_ipc::mcp::RequestId::String("request-1".to_string());
    store.push_event(elicitation_event("form", "server-1", request_id.clone()));
    assert_eq!(snapshot_ids(&store), ["patch", "form"]);
    store.note_outbound_op(&Op::PatchApproval {
        id: "patch-call".to_string(),
        decision: chaos_ipc::protocol::ReviewDecision::Approved,
    });
    assert_eq!(snapshot_ids(&store), ["form"]);
    store.note_outbound_op(&Op::ResolveElicitation {
        server_name: "server-1".to_string(),
        request_id,
        decision: chaos_ipc::approvals::ElicitationAction::Accept,
        content: None,
        meta: None,
    });
    assert!(snapshot_ids(&store).is_empty());
    assert!(!store.has_pending_process_approvals());
}

#[test]
fn turn_abort_filters_pending_turn_scoped_approval_prompts() {
    let mut store = ProcessEventStore::new(8);
    store.push_event(exec_approval_event(
        "ev-1",
        "exec-call-1",
        Some("approval-1"),
        "turn-1",
    ));
    store.push_event(patch_approval_event("ev-2", "patch-call-1", "turn-1"));
    store.push_event(patch_approval_event("other", "patch-call-2", "turn-2"));
    assert_eq!(snapshot_ids(&store), ["ev-1", "ev-2", "other"]);
    store.push_event(Event {
        id: "ev-3".to_string(),
        msg: EventMsg::TurnAborted(chaos_ipc::protocol::TurnAbortedEvent {
            turn_id: Some("turn-1".to_string()),
            reason: TurnAbortReason::Replaced,
        }),
    });

    assert_eq!(snapshot_ids(&store), ["other", "ev-3"]);
}
