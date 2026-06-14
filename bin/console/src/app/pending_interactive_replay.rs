use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::Op;
use std::collections::HashMap;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ElicitationRequestKey {
    server_name: String,
    request_id: chaos_ipc::mcp::RequestId,
}

impl ElicitationRequestKey {
    fn new(server_name: String, request_id: chaos_ipc::mcp::RequestId) -> Self {
        Self {
            server_name,
            request_id,
        }
    }
}

#[derive(Debug, Default)]
// Tracks which interactive prompts are still unresolved in the thread-event buffer.
//
// Thread snapshots are replayed when switching threads/agents. Most events should replay
// verbatim, but interactive prompts (approvals, request_user_input, MCP elicitations) must
// only replay if they are still pending. This state is updated from:
// - inbound events (`note_event`)
// - outbound ops that resolve a prompt (`note_outbound_op`)
// - buffer eviction (`note_evicted_event`)
//
// We keep both fast lookup sets (for snapshot filtering by call_id/request key) and
// turn-indexed queues/vectors so `TurnComplete`/`TurnAborted` can clear stale prompts tied
// to a turn. `request_user_input` removal is FIFO because the overlay answers queued prompts
// in FIFO order for a shared `turn_id`.
pub(super) struct PendingInteractiveReplayState {
    exec_approval_call_ids: HashSet<String>,
    exec_approval_call_ids_by_turn_id: HashMap<String, Vec<String>>,
    patch_approval_call_ids: HashSet<String>,
    patch_approval_call_ids_by_turn_id: HashMap<String, Vec<String>>,
    elicitation_requests: HashSet<ElicitationRequestKey>,
    request_permissions_call_ids: HashSet<String>,
    request_permissions_call_ids_by_turn_id: HashMap<String, Vec<String>>,
    request_user_input_call_ids: HashSet<String>,
    request_user_input_call_ids_by_turn_id: HashMap<String, Vec<String>>,
}

impl PendingInteractiveReplayState {
    pub(super) fn event_can_change_pending_process_approvals(event: &Event) -> bool {
        matches!(
            &event.msg,
            EventMsg::ExecApprovalRequest(_)
                | EventMsg::ApplyPatchApprovalRequest(_)
                | EventMsg::ElicitationRequest(_)
                | EventMsg::RequestPermissions(_)
                | EventMsg::ExecCommandBegin(_)
                | EventMsg::PatchApplyBegin(_)
                | EventMsg::TurnComplete(_)
                | EventMsg::TurnAborted(_)
                | EventMsg::ShutdownComplete
        )
    }

    pub(super) fn op_can_change_state(op: &Op) -> bool {
        matches!(
            op,
            Op::ExecApproval { .. }
                | Op::PatchApproval { .. }
                | Op::ResolveElicitation { .. }
                | Op::RequestPermissionsResponse { .. }
                | Op::UserInputAnswer { .. }
                | Op::Shutdown
        )
    }

    pub(super) fn note_outbound_op(&mut self, op: &Op) {
        match op {
            Op::ExecApproval { id, turn_id, .. } => {
                self.exec_approval_call_ids.remove(id);
                if let Some(turn_id) = turn_id {
                    Self::remove_call_id_from_turn_map_entry(
                        &mut self.exec_approval_call_ids_by_turn_id,
                        turn_id,
                        id,
                    );
                }
            }
            Op::PatchApproval { id, .. } => {
                self.patch_approval_call_ids.remove(id);
                Self::remove_call_id_from_turn_map(
                    &mut self.patch_approval_call_ids_by_turn_id,
                    id,
                );
            }
            Op::ResolveElicitation {
                server_name,
                request_id,
                ..
            } => {
                self.elicitation_requests
                    .remove(&ElicitationRequestKey::new(
                        server_name.clone(),
                        request_id.clone(),
                    ));
            }
            Op::RequestPermissionsResponse { id, .. } => {
                self.request_permissions_call_ids.remove(id);
                Self::remove_call_id_from_turn_map(
                    &mut self.request_permissions_call_ids_by_turn_id,
                    id,
                );
            }
            // `Op::UserInputAnswer` identifies the turn, not the prompt call_id. The UI
            // answers queued prompts for the same turn in FIFO order, so remove the oldest
            // queued call_id for that turn.
            Op::UserInputAnswer { id, .. } => {
                let mut remove_turn_entry = false;
                if let Some(call_ids) = self.request_user_input_call_ids_by_turn_id.get_mut(id) {
                    if !call_ids.is_empty() {
                        let call_id = call_ids.remove(0);
                        self.request_user_input_call_ids.remove(&call_id);
                    }
                    if call_ids.is_empty() {
                        remove_turn_entry = true;
                    }
                }
                if remove_turn_entry {
                    self.request_user_input_call_ids_by_turn_id.remove(id);
                }
            }
            Op::Shutdown => self.clear(),
            _ => {}
        }
    }

    pub(super) fn note_event(&mut self, event: &Event) {
        match &event.msg {
            EventMsg::ExecApprovalRequest(ev) => {
                let approval_id = ev.effective_approval_id();
                self.exec_approval_call_ids.insert(approval_id.clone());
                self.exec_approval_call_ids_by_turn_id
                    .entry(ev.turn_id.clone())
                    .or_default()
                    .push(approval_id);
            }
            EventMsg::ExecCommandBegin(ev) => {
                self.exec_approval_call_ids.remove(&ev.call_id);
                Self::remove_call_id_from_turn_map(
                    &mut self.exec_approval_call_ids_by_turn_id,
                    &ev.call_id,
                );
            }
            EventMsg::ApplyPatchApprovalRequest(ev) => {
                self.patch_approval_call_ids.insert(ev.call_id.clone());
                self.patch_approval_call_ids_by_turn_id
                    .entry(ev.turn_id.clone())
                    .or_default()
                    .push(ev.call_id.clone());
            }
            EventMsg::PatchApplyBegin(ev) => {
                self.patch_approval_call_ids.remove(&ev.call_id);
                Self::remove_call_id_from_turn_map(
                    &mut self.patch_approval_call_ids_by_turn_id,
                    &ev.call_id,
                );
            }
            EventMsg::ElicitationRequest(ev) => {
                self.elicitation_requests.insert(ElicitationRequestKey::new(
                    ev.server_name.clone(),
                    ev.id.clone(),
                ));
            }
            EventMsg::RequestUserInput(ev) => {
                self.request_user_input_call_ids.insert(ev.call_id.clone());
                self.request_user_input_call_ids_by_turn_id
                    .entry(ev.turn_id.clone())
                    .or_default()
                    .push(ev.call_id.clone());
            }
            EventMsg::RequestPermissions(ev) => {
                self.request_permissions_call_ids.insert(ev.call_id.clone());
                self.request_permissions_call_ids_by_turn_id
                    .entry(ev.turn_id.clone())
                    .or_default()
                    .push(ev.call_id.clone());
            }
            // A turn ending (normally or aborted/replaced) invalidates any unresolved
            // turn-scoped approvals, permission prompts, and request_user_input prompts.
            EventMsg::TurnComplete(ev) => {
                self.clear_exec_approval_turn(&ev.turn_id);
                self.clear_patch_approval_turn(&ev.turn_id);
                self.clear_request_permissions_turn(&ev.turn_id);
                self.clear_request_user_input_turn(&ev.turn_id);
            }
            EventMsg::TurnAborted(ev) => {
                if let Some(turn_id) = &ev.turn_id {
                    self.clear_exec_approval_turn(turn_id);
                    self.clear_patch_approval_turn(turn_id);
                    self.clear_request_permissions_turn(turn_id);
                    self.clear_request_user_input_turn(turn_id);
                }
            }
            EventMsg::ShutdownComplete => self.clear(),
            _ => {}
        }
    }

    pub(super) fn note_evicted_event(&mut self, event: &Event) {
        match &event.msg {
            EventMsg::ExecApprovalRequest(ev) => {
                let approval_id = ev.effective_approval_id();
                self.exec_approval_call_ids.remove(&approval_id);
                Self::remove_call_id_from_turn_map_entry(
                    &mut self.exec_approval_call_ids_by_turn_id,
                    &ev.turn_id,
                    &approval_id,
                );
            }
            EventMsg::ApplyPatchApprovalRequest(ev) => {
                self.patch_approval_call_ids.remove(&ev.call_id);
                Self::remove_call_id_from_turn_map_entry(
                    &mut self.patch_approval_call_ids_by_turn_id,
                    &ev.turn_id,
                    &ev.call_id,
                );
            }
            EventMsg::ElicitationRequest(ev) => {
                self.elicitation_requests
                    .remove(&ElicitationRequestKey::new(
                        ev.server_name.clone(),
                        ev.id.clone(),
                    ));
            }
            EventMsg::RequestUserInput(ev) => {
                self.request_user_input_call_ids.remove(&ev.call_id);
                let mut remove_turn_entry = false;
                if let Some(call_ids) = self
                    .request_user_input_call_ids_by_turn_id
                    .get_mut(&ev.turn_id)
                {
                    call_ids.retain(|call_id| call_id != &ev.call_id);
                    if call_ids.is_empty() {
                        remove_turn_entry = true;
                    }
                }
                if remove_turn_entry {
                    self.request_user_input_call_ids_by_turn_id
                        .remove(&ev.turn_id);
                }
            }
            EventMsg::RequestPermissions(ev) => {
                self.request_permissions_call_ids.remove(&ev.call_id);
                let mut remove_turn_entry = false;
                if let Some(call_ids) = self
                    .request_permissions_call_ids_by_turn_id
                    .get_mut(&ev.turn_id)
                {
                    call_ids.retain(|call_id| call_id != &ev.call_id);
                    if call_ids.is_empty() {
                        remove_turn_entry = true;
                    }
                }
                if remove_turn_entry {
                    self.request_permissions_call_ids_by_turn_id
                        .remove(&ev.turn_id);
                }
            }
            _ => {}
        }
    }

    pub(super) fn should_replay_snapshot_event(&self, event: &Event) -> bool {
        match &event.msg {
            EventMsg::ExecApprovalRequest(ev) => self
                .exec_approval_call_ids
                .contains(&ev.effective_approval_id()),
            EventMsg::ApplyPatchApprovalRequest(ev) => {
                self.patch_approval_call_ids.contains(&ev.call_id)
            }
            EventMsg::ElicitationRequest(ev) => {
                self.elicitation_requests
                    .contains(&ElicitationRequestKey::new(
                        ev.server_name.clone(),
                        ev.id.clone(),
                    ))
            }
            EventMsg::RequestUserInput(ev) => {
                self.request_user_input_call_ids.contains(&ev.call_id)
            }
            EventMsg::RequestPermissions(ev) => {
                self.request_permissions_call_ids.contains(&ev.call_id)
            }
            _ => true,
        }
    }

    pub(super) fn has_pending_process_approvals(&self) -> bool {
        !self.exec_approval_call_ids.is_empty()
            || !self.patch_approval_call_ids.is_empty()
            || !self.elicitation_requests.is_empty()
            || !self.request_permissions_call_ids.is_empty()
    }

    fn clear_request_user_input_turn(&mut self, turn_id: &str) {
        if let Some(call_ids) = self.request_user_input_call_ids_by_turn_id.remove(turn_id) {
            for call_id in call_ids {
                self.request_user_input_call_ids.remove(&call_id);
            }
        }
    }

    fn clear_request_permissions_turn(&mut self, turn_id: &str) {
        if let Some(call_ids) = self.request_permissions_call_ids_by_turn_id.remove(turn_id) {
            for call_id in call_ids {
                self.request_permissions_call_ids.remove(&call_id);
            }
        }
    }

    fn clear_exec_approval_turn(&mut self, turn_id: &str) {
        if let Some(call_ids) = self.exec_approval_call_ids_by_turn_id.remove(turn_id) {
            for call_id in call_ids {
                self.exec_approval_call_ids.remove(&call_id);
            }
        }
    }

    fn clear_patch_approval_turn(&mut self, turn_id: &str) {
        if let Some(call_ids) = self.patch_approval_call_ids_by_turn_id.remove(turn_id) {
            for call_id in call_ids {
                self.patch_approval_call_ids.remove(&call_id);
            }
        }
    }

    fn remove_call_id_from_turn_map(
        call_ids_by_turn_id: &mut HashMap<String, Vec<String>>,
        call_id: &str,
    ) {
        call_ids_by_turn_id.retain(|_, call_ids| {
            call_ids.retain(|queued_call_id| queued_call_id != call_id);
            !call_ids.is_empty()
        });
    }

    fn remove_call_id_from_turn_map_entry(
        call_ids_by_turn_id: &mut HashMap<String, Vec<String>>,
        turn_id: &str,
        call_id: &str,
    ) {
        let mut remove_turn_entry = false;
        if let Some(call_ids) = call_ids_by_turn_id.get_mut(turn_id) {
            call_ids.retain(|queued_call_id| queued_call_id != call_id);
            if call_ids.is_empty() {
                remove_turn_entry = true;
            }
        }
        if remove_turn_entry {
            call_ids_by_turn_id.remove(turn_id);
        }
    }

    fn clear(&mut self) {
        self.exec_approval_call_ids.clear();
        self.exec_approval_call_ids_by_turn_id.clear();
        self.patch_approval_call_ids.clear();
        self.patch_approval_call_ids_by_turn_id.clear();
        self.elicitation_requests.clear();
        self.request_permissions_call_ids.clear();
        self.request_permissions_call_ids_by_turn_id.clear();
        self.request_user_input_call_ids.clear();
        self.request_user_input_call_ids_by_turn_id.clear();
    }
}

#[cfg(test)]
pub(crate) mod tests {
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
}
