//! Turn-scoped state and active turn metadata scaffolding.

use indexmap::IndexMap;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use chaos_ipc::dynamic_tools::DynamicToolResponse;
use chaos_ipc::models::ResponseInputItem;
use chaos_ipc::request_permissions::RequestPermissionsResponse;
use chaos_ipc::request_user_input::RequestUserInputResponse;
use mcp_guest::protocol::ElicitationResponse;
use mcp_guest::protocol::RequestId;
use tokio::sync::oneshot;

use crate::chaos::TurnContext;
use crate::protocol::ReviewDecision;
use crate::protocol::TokenUsage;
use crate::sandboxing::merge_permission_profiles;
use crate::tasks::SessionTask;
use chaos_ipc::models::PermissionProfile;

/// Metadata about the currently running turn.
pub(crate) struct ActiveTurn {
    pub(crate) tasks: IndexMap<String, RunningTask>,
    pub(crate) turn_state: Arc<Mutex<TurnState>>,
}

impl Default for ActiveTurn {
    fn default() -> Self {
        Self {
            tasks: IndexMap::new(),
            turn_state: Arc::new(Mutex::new(TurnState::default())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TaskKind {
    Regular,
    Review,
    Compact,
}

pub(crate) struct RunningTask {
    pub(crate) done: Arc<Notify>,
    pub(crate) kind: TaskKind,
    pub(crate) task: Arc<dyn SessionTask>,
    pub(crate) cancellation_token: CancellationToken,
    pub(crate) handle: Arc<AbortOnDropHandle<()>>,
    pub(crate) turn_context: Arc<TurnContext>,
    // Timer recorded when the task drops to capture the full turn duration.
    pub(crate) _timer: Option<chaos_syslog::Timer>,
}

impl ActiveTurn {
    pub(crate) fn add_task(&mut self, task: RunningTask) {
        let sub_id = task.turn_context.sub_id.clone();
        self.tasks.insert(sub_id, task);
    }

    pub(crate) fn remove_task(&mut self, sub_id: &str) -> bool {
        self.tasks.swap_remove(sub_id);
        self.tasks.is_empty()
    }

    pub(crate) fn drain_tasks(&mut self) -> Vec<RunningTask> {
        self.tasks.drain(..).map(|(_, task)| task).collect()
    }
}

/// Tracks whether pending mailbox input should be delivered to the
/// current turn or held for the next one.
///
/// The state machine has two states:
///
/// - `CurrentTurn` (default) — mailbox items are deliverable now.
/// - `NextTurn` — the model emitted a final answer; hold items for
///   the next turn.
///
/// Transitions:
/// - `record_answer_emitted()` → `NextTurn` (guarded: only if the
///   mailbox is empty; if input already arrived, stay `CurrentTurn`)
/// - `record_tool_call_emitted()` → `CurrentTurn` (tool calls
///   reopen the turn)
/// - `record_steered_input()` → `CurrentTurn` (new user input
///   always reopens delivery; called atomically from
///   `push_pending_input`)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MailboxDeliveryPhase {
    /// Mailbox items are deliverable to the current turn.
    #[default]
    CurrentTurn,
    /// The model already emitted a final answer; defer mailbox items
    /// to the next turn.
    NextTurn,
}

/// Mutable state for a single turn.
#[derive(Default)]
pub(crate) struct TurnState {
    pending_approvals: HashMap<String, oneshot::Sender<ReviewDecision>>,
    pending_request_permissions: HashMap<String, oneshot::Sender<RequestPermissionsResponse>>,
    pending_user_input: HashMap<String, oneshot::Sender<RequestUserInputResponse>>,
    pending_elicitations: HashMap<(String, RequestId), oneshot::Sender<ElicitationResponse>>,
    pending_dynamic_tools: HashMap<String, oneshot::Sender<DynamicToolResponse>>,
    pending_input: Vec<ResponseInputItem>,
    mailbox_delivery_phase: MailboxDeliveryPhase,
    granted_permissions: Option<PermissionProfile>,
    pub(crate) tool_calls: u64,
    pub(crate) token_usage_at_turn_start: TokenUsage,
}

impl TurnState {
    pub(crate) fn insert_pending_approval(
        &mut self,
        key: String,
        tx: oneshot::Sender<ReviewDecision>,
    ) -> Option<oneshot::Sender<ReviewDecision>> {
        self.pending_approvals.insert(key, tx)
    }

    pub(crate) fn remove_pending_approval(
        &mut self,
        key: &str,
    ) -> Option<oneshot::Sender<ReviewDecision>> {
        self.pending_approvals.remove(key)
    }

    pub(crate) fn clear_pending(&mut self) {
        self.pending_approvals.clear();
        self.pending_request_permissions.clear();
        self.pending_user_input.clear();
        self.pending_elicitations.clear();
        self.pending_dynamic_tools.clear();
        self.pending_input.clear();
    }

    pub(crate) fn insert_pending_request_permissions(
        &mut self,
        key: String,
        tx: oneshot::Sender<RequestPermissionsResponse>,
    ) -> Option<oneshot::Sender<RequestPermissionsResponse>> {
        self.pending_request_permissions.insert(key, tx)
    }

    pub(crate) fn remove_pending_request_permissions(
        &mut self,
        key: &str,
    ) -> Option<oneshot::Sender<RequestPermissionsResponse>> {
        self.pending_request_permissions.remove(key)
    }

    pub(crate) fn insert_pending_user_input(
        &mut self,
        key: String,
        tx: oneshot::Sender<RequestUserInputResponse>,
    ) -> Option<oneshot::Sender<RequestUserInputResponse>> {
        self.pending_user_input.insert(key, tx)
    }

    pub(crate) fn remove_pending_user_input(
        &mut self,
        key: &str,
    ) -> Option<oneshot::Sender<RequestUserInputResponse>> {
        self.pending_user_input.remove(key)
    }

    pub(crate) fn insert_pending_elicitation(
        &mut self,
        server_name: String,
        request_id: RequestId,
        tx: oneshot::Sender<ElicitationResponse>,
    ) -> Option<oneshot::Sender<ElicitationResponse>> {
        self.pending_elicitations
            .insert((server_name, request_id), tx)
    }

    pub(crate) fn remove_pending_elicitation(
        &mut self,
        server_name: &str,
        request_id: &RequestId,
    ) -> Option<oneshot::Sender<ElicitationResponse>> {
        self.pending_elicitations
            .remove(&(server_name.to_string(), request_id.clone()))
    }

    pub(crate) fn insert_pending_dynamic_tool(
        &mut self,
        key: String,
        tx: oneshot::Sender<DynamicToolResponse>,
    ) -> Option<oneshot::Sender<DynamicToolResponse>> {
        self.pending_dynamic_tools.insert(key, tx)
    }

    pub(crate) fn remove_pending_dynamic_tool(
        &mut self,
        key: &str,
    ) -> Option<oneshot::Sender<DynamicToolResponse>> {
        self.pending_dynamic_tools.remove(key)
    }

    // ── Mailbox delivery phase ────────────────────────────────────

    /// Returns `true` when mailbox items should be delivered to the
    /// current turn.
    pub(crate) fn accepts_mailbox_delivery(&self) -> bool {
        self.mailbox_delivery_phase == MailboxDeliveryPhase::CurrentTurn
    }

    /// Called when the model completes a final answer item. Defers
    /// mailbox delivery to the next turn, but only when the mailbox
    /// is empty — if the user has already steered input the turn
    /// must stay open.
    pub(crate) fn record_answer_emitted(&mut self) {
        if self.pending_input.is_empty() {
            self.mailbox_delivery_phase = MailboxDeliveryPhase::NextTurn;
        }
        // If pending_input is non-empty, the user steered input
        // before the answer boundary; keep CurrentTurn so it is
        // consumed on the next follow-up iteration.
    }

    /// Called when a tool call is emitted. Reopens mailbox delivery
    /// for the current turn.
    pub(crate) fn record_tool_call_emitted(&mut self) {
        self.mailbox_delivery_phase = MailboxDeliveryPhase::CurrentTurn;
    }

    /// Called atomically from `push_pending_input`. Any steered
    /// input reopens delivery for the current turn, preventing a
    /// stale `NextTurn` phase from hiding the new input.
    fn record_steered_input(&mut self) {
        self.mailbox_delivery_phase = MailboxDeliveryPhase::CurrentTurn;
    }

    // ── Pending input ─────────────────────────────────────────────

    /// Push an item into the mailbox and atomically reopen mailbox
    /// delivery for the current turn.
    pub(crate) fn push_pending_input(&mut self, input: ResponseInputItem) {
        self.pending_input.push(input);
        self.record_steered_input();
    }

    /// Drain and return all pending items regardless of phase. Used
    /// at the start of the next turn after `needs_follow_up` is set.
    pub(crate) fn take_pending_input(&mut self) -> Vec<ResponseInputItem> {
        if self.pending_input.is_empty() {
            Vec::with_capacity(0)
        } else {
            let mut ret = Vec::new();
            std::mem::swap(&mut ret, &mut self.pending_input);
            ret
        }
    }

    /// `true` if there are pending items AND the phase allows
    /// delivery to the current turn.
    pub(crate) fn has_deliverable_input(&self) -> bool {
        self.accepts_mailbox_delivery() && !self.pending_input.is_empty()
    }

    pub(crate) fn record_granted_permissions(&mut self, permissions: PermissionProfile) {
        self.granted_permissions =
            merge_permission_profiles(self.granted_permissions.as_ref(), Some(&permissions));
    }

    pub(crate) fn granted_permissions(&self) -> Option<PermissionProfile> {
        self.granted_permissions.clone()
    }
}

impl ActiveTurn {
    /// Clear any pending approvals and input buffered for the current turn.
    pub(crate) async fn clear_pending(&self) {
        let mut ts = self.turn_state.lock().await;
        ts.clear_pending();
    }
}

#[cfg(test)]
mod tests {
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
    fn defaults_to_current_turn() {
        let ts = TurnState::default();
        assert!(ts.accepts_mailbox_delivery());
        assert!(!ts.has_deliverable_input());
    }

    #[test]
    fn answer_emitted_defers_when_mailbox_empty() {
        let mut ts = TurnState::default();
        ts.record_answer_emitted();
        assert!(!ts.accepts_mailbox_delivery());
        assert!(!ts.has_deliverable_input());
    }

    #[test]
    fn steered_input_reopens_delivery() {
        let mut ts = TurnState::default();
        ts.record_answer_emitted();
        assert!(!ts.accepts_mailbox_delivery());

        ts.push_pending_input(make_input());
        // push_pending_input atomically calls record_steered_input
        assert!(ts.accepts_mailbox_delivery());
        assert!(ts.has_deliverable_input());
    }

    #[test]
    fn tool_call_reopens_delivery() {
        let mut ts = TurnState::default();
        ts.record_answer_emitted();
        assert!(!ts.accepts_mailbox_delivery());

        ts.record_tool_call_emitted();
        assert!(ts.accepts_mailbox_delivery());
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
}
