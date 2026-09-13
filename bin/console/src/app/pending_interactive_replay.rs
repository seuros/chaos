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
pub(crate) mod tests;
