use super::*;

impl App {
    pub(super) async fn interactive_request_for_process_event(
        &self,
        process_id: ProcessId,
        event: &Event,
    ) -> Option<ProcessInteractiveRequest> {
        let process_label = Some(self.process_label(process_id));
        match &event.msg {
            EventMsg::ExecApprovalRequest(ev) => {
                Some(ProcessInteractiveRequest::Approval(ApprovalRequest::Exec {
                    process_id,
                    process_label,
                    id: ev.effective_approval_id(),
                    command: ev.command.clone(),
                    reason: ev.reason.clone(),
                    available_decisions: ev.effective_available_decisions(),
                    network_approval_context: ev.network_approval_context.clone(),
                    additional_permissions: ev.additional_permissions.clone(),
                }))
            }
            EventMsg::ApplyPatchApprovalRequest(ev) => Some(ProcessInteractiveRequest::Approval(
                ApprovalRequest::ApplyPatch {
                    process_id,
                    process_label,
                    id: ev.call_id.clone(),
                    reason: ev.reason.clone(),
                    cwd: self
                        .process_cwd(process_id)
                        .await
                        .unwrap_or_else(|| self.config.cwd.clone()),
                    changes: ev.changes.clone(),
                },
            )),
            EventMsg::ElicitationRequest(ev) => {
                if let Some(request) =
                    McpServerElicitationFormRequest::from_event(process_id, ev.clone())
                {
                    Some(ProcessInteractiveRequest::McpServerElicitation(request))
                } else {
                    let url = match &ev.request {
                        chaos_ipc::approvals::ElicitationRequest::Url { url, .. } => {
                            Some(url.clone())
                        }
                        chaos_ipc::approvals::ElicitationRequest::Form { .. } => None,
                    };
                    Some(ProcessInteractiveRequest::Approval(
                        ApprovalRequest::McpElicitation {
                            process_id,
                            process_label,
                            server_name: ev.server_name.clone(),
                            request_id: ev.id.clone(),
                            message: ev.request.message().to_string(),
                            url,
                        },
                    ))
                }
            }
            EventMsg::RequestPermissions(ev) => Some(ProcessInteractiveRequest::Approval(
                ApprovalRequest::Permissions {
                    process_id,
                    process_label,
                    call_id: ev.call_id.clone(),
                    reason: ev.reason.clone(),
                    permissions: ev.permissions.clone(),
                },
            )),
            _ => None,
        }
    }

    pub(super) async fn submit_op_to_process(&mut self, process_id: ProcessId, op: Op) {
        let replay_state_op =
            ProcessEventStore::op_can_change_pending_replay_state(&op).then(|| op.clone());
        let submitted = if self.active_process_id == Some(process_id) {
            self.chat_widget.submit_op(op)
        } else {
            crate::session_log::log_outbound_op(&op);
            match self.server.get_process(process_id).await {
                Ok(thread) => match thread.submit(op).await {
                    Ok(_) => true,
                    Err(err) => {
                        self.chat_widget.add_error_message(format!(
                            "Failed to submit op to process {process_id}: {err}"
                        ));
                        false
                    }
                },
                Err(err) => {
                    self.chat_widget.add_error_message(format!(
                        "Failed to find process {process_id} for approval response: {err}"
                    ));
                    false
                }
            }
        };
        if submitted && let Some(op) = replay_state_op.as_ref() {
            self.note_process_outbound_op(process_id, op).await;
            self.refresh_pending_process_approvals().await;
        }
    }

    pub(super) async fn refresh_pending_process_approvals(&mut self) {
        let channels: Vec<(ProcessId, Arc<Mutex<ProcessEventStore>>)> = self
            .process_event_channels
            .iter()
            .map(|(process_id, channel)| (*process_id, Arc::clone(&channel.store)))
            .collect();

        let mut pending_process_ids = Vec::new();
        for (process_id, store) in channels {
            if Some(process_id) == self.active_process_id {
                continue;
            }

            let store = store.lock().await;
            if store.has_pending_process_approvals() {
                pending_process_ids.push(process_id);
            }
        }

        pending_process_ids.sort_by_key(ProcessId::to_string);

        let threads = pending_process_ids
            .into_iter()
            .map(|process_id| self.process_label(process_id))
            .collect();

        self.chat_widget.set_pending_process_approvals(threads);
    }
}
