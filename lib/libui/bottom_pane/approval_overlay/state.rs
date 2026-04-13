use chaos_ipc::mcp::RequestId;
use chaos_ipc::protocol::ElicitationAction;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::ReviewDecision;
use chaos_ipc::request_permissions::PermissionGrantScope;
use chaos_kern::features::Features;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::list_selection_view::ListSelectionView;
use crate::bottom_pane::list_selection_view::SelectionItem;
use crate::bottom_pane::list_selection_view::SelectionViewParams;
use crate::history_cell;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;

use super::super::BottomPaneView;
use super::super::CancellationEvent;
use super::request::ApprovalDecision;
use super::request::ApprovalOption;
use super::request::ApprovalRequest;
use super::request::approval_footer_hint;
use super::request::build_header;
use super::request::elicitation_options;
use super::request::exec_options;
use super::request::patch_options;
use super::request::permissions_options;

/// Modal overlay asking the user to approve or deny one or more requests.
pub struct ApprovalOverlay {
    pub(super) current_request: Option<ApprovalRequest>,
    pub(super) queue: Vec<ApprovalRequest>,
    app_event_tx: AppEventSender,
    list: ListSelectionView,
    options: Vec<ApprovalOption>,
    current_complete: bool,
    pub(super) done: bool,
    features: Features,
}

impl ApprovalOverlay {
    pub fn new(request: ApprovalRequest, app_event_tx: AppEventSender, features: Features) -> Self {
        let mut view = Self {
            current_request: None,
            queue: Vec::new(),
            app_event_tx: app_event_tx.clone(),
            list: ListSelectionView::new(Default::default(), app_event_tx),
            options: Vec::new(),
            current_complete: false,
            done: false,
            features,
        };
        view.set_current(request);
        view
    }

    pub fn enqueue_request(&mut self, req: ApprovalRequest) {
        self.queue.push(req);
    }

    fn set_current(&mut self, request: ApprovalRequest) {
        self.current_complete = false;
        let header = build_header(&request);
        let (options, params) = Self::build_options(&request, header, &self.features);
        self.current_request = Some(request);
        self.options = options;
        self.list = ListSelectionView::new(params, self.app_event_tx.clone());
    }

    fn build_options(
        request: &ApprovalRequest,
        header: Box<dyn Renderable>,
        _features: &Features,
    ) -> (Vec<ApprovalOption>, SelectionViewParams) {
        let (options, title) = match request {
            ApprovalRequest::Exec {
                available_decisions,
                network_approval_context,
                additional_permissions,
                ..
            } => (
                exec_options(
                    available_decisions,
                    network_approval_context.as_ref(),
                    additional_permissions.as_ref(),
                ),
                network_approval_context.as_ref().map_or_else(
                    || "Would you like to run the following command?".to_string(),
                    |network_approval_context| {
                        format!(
                            "Do you want to approve network access to \"{}\"?",
                            network_approval_context.host
                        )
                    },
                ),
            ),
            ApprovalRequest::Permissions { .. } => (
                permissions_options(),
                "Would you like to grant these permissions?".to_string(),
            ),
            ApprovalRequest::ApplyPatch { .. } => (
                patch_options(),
                "Would you like to make the following edits?".to_string(),
            ),
            ApprovalRequest::McpElicitation {
                server_name, url, ..
            } => (
                elicitation_options(url.is_some()),
                if url.is_some() {
                    format!("{server_name} wants to open a browser link.")
                } else {
                    format!("{server_name} needs your approval.")
                },
            ),
        };

        let header = Box::new(ColumnRenderable::with([
            Line::from(title.bold()).into(),
            Line::from("").into(),
            header,
        ]));

        use ratatui::style::Stylize;
        let items = options
            .iter()
            .map(|opt| SelectionItem {
                name: opt.label.clone(),
                display_shortcut: opt
                    .display_shortcut
                    .or_else(|| opt.additional_shortcuts.first().copied()),
                dismiss_on_select: false,
                ..Default::default()
            })
            .collect();

        let params = SelectionViewParams {
            footer_hint: Some(approval_footer_hint(request)),
            items,
            header,
            ..Default::default()
        };

        (options, params)
    }

    fn apply_selection(&mut self, actual_idx: usize) {
        if self.current_complete {
            return;
        }
        let Some(option) = self.options.get(actual_idx) else {
            return;
        };
        if let Some(request) = self.current_request.as_ref() {
            match (request, &option.decision) {
                (ApprovalRequest::Exec { id, command, .. }, ApprovalDecision::Review(decision)) => {
                    self.handle_exec_decision(id, command, decision.clone());
                }
                (
                    ApprovalRequest::Permissions {
                        call_id,
                        permissions,
                        ..
                    },
                    ApprovalDecision::Review(decision),
                ) => self.handle_permissions_decision(call_id, permissions, decision.clone()),
                (ApprovalRequest::ApplyPatch { id, .. }, ApprovalDecision::Review(decision)) => {
                    self.handle_patch_decision(id, decision.clone());
                }
                (
                    ApprovalRequest::McpElicitation {
                        process_id,
                        server_name,
                        request_id,
                        url,
                        ..
                    },
                    ApprovalDecision::McpElicitation(decision),
                ) => {
                    if matches!(decision, ElicitationAction::Accept)
                        && let Some(url) = url.as_ref()
                    {
                        self.app_event_tx
                            .send(AppEvent::OpenUrlElicitationInBrowser {
                                process_id: *process_id,
                                server_name: server_name.clone(),
                                request_id: request_id.clone(),
                                url: url.clone(),
                                on_open: ElicitationAction::Accept,
                                on_error: ElicitationAction::Cancel,
                            });
                    } else {
                        self.handle_elicitation_decision(server_name, request_id, *decision);
                    }
                }
                _ => {}
            }
        }

        self.current_complete = true;
        self.advance_queue();
    }

    fn handle_exec_decision(&self, id: &str, command: &[String], decision: ReviewDecision) {
        let Some(request) = self.current_request.as_ref() else {
            return;
        };
        if request.process_label().is_none() {
            let cell = history_cell::new_approval_decision_cell(
                command.to_vec(),
                decision.clone(),
                history_cell::ApprovalDecisionActor::User,
            );
            self.app_event_tx.send(AppEvent::InsertHistoryCell(cell));
        }
        let process_id = request.process_id();
        self.app_event_tx.send(AppEvent::SubmitProcessOp {
            process_id,
            op: Op::ExecApproval {
                id: id.to_string(),
                turn_id: None,
                decision,
            },
        });
    }

    fn handle_permissions_decision(
        &self,
        call_id: &str,
        permissions: &chaos_ipc::request_permissions::RequestPermissionProfile,
        decision: ReviewDecision,
    ) {
        let Some(request) = self.current_request.as_ref() else {
            return;
        };
        let granted_permissions = match decision {
            ReviewDecision::Approved | ReviewDecision::ApprovedForSession => permissions.clone(),
            ReviewDecision::Denied | ReviewDecision::Abort => Default::default(),
            ReviewDecision::ApprovedExecpolicyAmendment { .. }
            | ReviewDecision::NetworkPolicyAmendment { .. } => Default::default(),
        };
        let scope = if matches!(decision, ReviewDecision::ApprovedForSession) {
            PermissionGrantScope::Session
        } else {
            PermissionGrantScope::Turn
        };
        if request.process_label().is_none() {
            let message = if granted_permissions.is_empty() {
                "You did not grant additional permissions"
            } else if matches!(scope, PermissionGrantScope::Session) {
                "You granted additional permissions for this session"
            } else {
                "You granted additional permissions"
            };
            self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                crate::history_cell::PlainHistoryCell::new(vec![message.into()]),
            )));
        }
        let process_id = request.process_id();
        self.app_event_tx.send(AppEvent::SubmitProcessOp {
            process_id,
            op: Op::RequestPermissionsResponse {
                id: call_id.to_string(),
                response: chaos_ipc::request_permissions::RequestPermissionsResponse {
                    permissions: granted_permissions,
                    scope,
                },
            },
        });
    }

    fn handle_patch_decision(&self, id: &str, decision: ReviewDecision) {
        let Some(process_id) = self
            .current_request
            .as_ref()
            .map(ApprovalRequest::process_id)
        else {
            return;
        };
        self.app_event_tx.send(AppEvent::SubmitProcessOp {
            process_id,
            op: Op::PatchApproval {
                id: id.to_string(),
                decision,
            },
        });
    }

    fn handle_elicitation_decision(
        &self,
        server_name: &str,
        request_id: &RequestId,
        decision: ElicitationAction,
    ) {
        let Some(process_id) = self
            .current_request
            .as_ref()
            .map(ApprovalRequest::process_id)
        else {
            return;
        };
        self.app_event_tx.send(AppEvent::SubmitProcessOp {
            process_id,
            op: Op::ResolveElicitation {
                server_name: server_name.to_string(),
                request_id: request_id.clone(),
                decision,
                content: None,
                meta: None,
            },
        });
    }

    fn advance_queue(&mut self) {
        if let Some(next) = self.queue.pop() {
            self.set_current(next);
        } else {
            self.done = true;
        }
    }

    fn try_handle_shortcut(&mut self, key_event: &KeyEvent) -> bool {
        match key_event {
            KeyEvent {
                kind: KeyEventKind::Press,
                code: KeyCode::Char('a'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(request) = self.current_request.as_ref() {
                    self.app_event_tx
                        .send(AppEvent::FullScreenApprovalRequest(request.clone()));
                    true
                } else {
                    false
                }
            }
            KeyEvent {
                kind: KeyEventKind::Press,
                code: KeyCode::Char('o'),
                ..
            } => {
                if let Some(request) = self.current_request.as_ref() {
                    if request.process_label().is_some() {
                        self.app_event_tx
                            .send(AppEvent::SelectAgentProcess(request.process_id()));
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            e => {
                if let Some(idx) = self
                    .options
                    .iter()
                    .position(|opt| opt.shortcuts().any(|s| s.is_press(*e)))
                {
                    self.apply_selection(idx);
                    true
                } else {
                    false
                }
            }
        }
    }
}

impl BottomPaneView for ApprovalOverlay {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if self.try_handle_shortcut(&key_event) {
            return;
        }
        self.list.handle_key_event(key_event);
        if let Some(idx) = self.list.take_last_selected_index() {
            self.apply_selection(idx);
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        if self.done {
            return CancellationEvent::Handled;
        }
        if !self.current_complete
            && let Some(request) = self.current_request.as_ref()
        {
            match request {
                ApprovalRequest::Exec { id, command, .. } => {
                    self.handle_exec_decision(id, command, ReviewDecision::Abort);
                }
                ApprovalRequest::Permissions {
                    call_id,
                    permissions,
                    ..
                } => {
                    self.handle_permissions_decision(call_id, permissions, ReviewDecision::Abort);
                }
                ApprovalRequest::ApplyPatch { id, .. } => {
                    self.handle_patch_decision(id, ReviewDecision::Abort);
                }
                ApprovalRequest::McpElicitation {
                    server_name,
                    request_id,
                    ..
                } => {
                    self.handle_elicitation_decision(
                        server_name,
                        request_id,
                        ElicitationAction::Cancel,
                    );
                }
            }
        }
        self.queue.clear();
        self.done = true;
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.done
    }

    fn try_consume_approval_request(
        &mut self,
        request: ApprovalRequest,
    ) -> Option<ApprovalRequest> {
        self.enqueue_request(request);
        None
    }
}

impl Renderable for ApprovalOverlay {
    fn desired_height(&self, width: u16) -> u16 {
        self.list.desired_height(width)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.list.render(area, buf);
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.list.cursor_pos(area)
    }
}
