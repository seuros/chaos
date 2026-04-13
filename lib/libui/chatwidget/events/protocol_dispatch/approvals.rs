//! Approval, elicitation, and permissions request event handlers.

use chaos_ipc::approvals::ElicitationRequestEvent;
use chaos_ipc::protocol::ApplyPatchApprovalRequestEvent;
use chaos_ipc::protocol::ExecApprovalRequestEvent;
use chaos_ipc::request_permissions::RequestPermissionsEvent;
use chaos_ipc::request_user_input::RequestUserInputEvent;

use super::super::super::ChatWidget;

impl ChatWidget {
    // ── Exec / patch approval events ──────────────────────────────────────────

    pub(crate) fn on_exec_approval_request(&mut self, _id: String, ev: ExecApprovalRequestEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_exec_approval(ev),
            |s| s.handle_exec_approval_now(ev2),
        );
    }

    pub(crate) fn on_apply_patch_approval_request(
        &mut self,
        _id: String,
        ev: ApplyPatchApprovalRequestEvent,
    ) {
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_apply_patch_approval(ev),
            |s| s.handle_apply_patch_approval_now(ev2),
        );
    }

    pub(crate) fn on_elicitation_request(&mut self, ev: ElicitationRequestEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_elicitation(ev),
            |s| s.handle_elicitation_request_now(ev2),
        );
    }

    pub(crate) fn on_request_user_input(&mut self, ev: RequestUserInputEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_user_input(ev),
            |s| s.handle_request_user_input_now(ev2),
        );
    }

    pub(crate) fn on_request_permissions(&mut self, ev: RequestPermissionsEvent) {
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_request_permissions(ev),
            |s| s.handle_request_permissions_now(ev2),
        );
    }
}
