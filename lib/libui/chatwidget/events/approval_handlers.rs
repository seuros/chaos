//! Approval-mode immediate-mode handlers for exec, patch, elicitation, user-input,
//! and permissions requests.

use chaos_ipc::protocol::ApplyPatchApprovalRequestEvent;
use chaos_ipc::protocol::ExecApprovalRequestEvent;
use chaos_ipc::request_permissions::RequestPermissionsEvent;
use chaos_ipc::request_user_input::RequestUserInputEvent;

use crate::bottom_pane::ApprovalRequest;
use crate::bottom_pane::McpServerElicitationFormRequest;

use super::super::ChatWidget;

impl ChatWidget {
    pub fn handle_exec_approval_now(&mut self, ev: ExecApprovalRequestEvent) {
        use super::super::core::Notification;
        self.flush_answer_stream_with_separator();
        let command = shlex::try_join(ev.command.iter().map(String::as_str))
            .unwrap_or_else(|_| ev.command.join(" "));
        self.notify(Notification::ExecApprovalRequested { command });
        let available_decisions = ev.effective_available_decisions();
        let request = ApprovalRequest::Exec {
            process_id: self.process_id.unwrap_or_default(),
            process_label: None,
            id: ev.effective_approval_id(),
            command: ev.command,
            reason: ev.reason,
            available_decisions,
            network_approval_context: ev.network_approval_context,
            additional_permissions: ev.additional_permissions,
        };
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
    }

    pub fn handle_apply_patch_approval_now(&mut self, ev: ApplyPatchApprovalRequestEvent) {
        use super::super::core::Notification;
        self.flush_answer_stream_with_separator();
        let request = ApprovalRequest::ApplyPatch {
            process_id: self.process_id.unwrap_or_default(),
            process_label: None,
            id: ev.call_id,
            reason: ev.reason,
            changes: ev.changes.clone(),
            cwd: self.config.cwd.clone(),
        };
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
        self.notify(Notification::EditApprovalRequested {
            cwd: self.config.cwd.clone(),
            changes: ev.changes.keys().cloned().collect(),
        });
    }

    pub fn handle_elicitation_request_now(
        &mut self,
        ev: chaos_ipc::approvals::ElicitationRequestEvent,
    ) {
        use super::super::core::Notification;
        use chaos_ipc::approvals::ElicitationRequest;
        self.flush_answer_stream_with_separator();
        self.notify(Notification::ElicitationRequested {
            server_name: ev.server_name.clone(),
        });
        let process_id = self.process_id.unwrap_or_default();
        if let Some(request) = McpServerElicitationFormRequest::from_event(process_id, ev.clone()) {
            self.bottom_pane
                .push_mcp_server_elicitation_request(request);
        } else {
            let url = match &ev.request {
                ElicitationRequest::Url { url, .. } => Some(url.clone()),
                ElicitationRequest::Form { .. } => None,
            };
            let request = ApprovalRequest::McpElicitation {
                process_id,
                process_label: None,
                server_name: ev.server_name,
                request_id: ev.id,
                message: ev.request.message().to_string(),
                url,
            };
            self.bottom_pane
                .push_approval_request(request, &self.config.features);
        }
        self.request_redraw();
    }

    pub fn push_approval_request(&mut self, request: ApprovalRequest) {
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
    }

    pub fn push_mcp_server_elicitation_request(
        &mut self,
        request: McpServerElicitationFormRequest,
    ) {
        self.bottom_pane
            .push_mcp_server_elicitation_request(request);
        self.request_redraw();
    }

    pub fn handle_request_user_input_now(&mut self, ev: RequestUserInputEvent) {
        use super::super::core::Notification;
        self.flush_answer_stream_with_separator();
        self.notify(Notification::UserInputRequested {
            question_count: ev.questions.len(),
            summary: Notification::user_input_request_summary(&ev.questions),
        });
        self.bottom_pane.push_user_input_request(ev);
        self.request_redraw();
    }

    pub fn handle_request_permissions_now(&mut self, ev: RequestPermissionsEvent) {
        self.flush_answer_stream_with_separator();
        let request = ApprovalRequest::Permissions {
            process_id: self.process_id.unwrap_or_default(),
            process_label: None,
            call_id: ev.call_id,
            reason: ev.reason,
            permissions: ev.permissions,
        };
        self.bottom_pane
            .push_approval_request(request, &self.config.features);
        self.request_redraw();
    }
}
