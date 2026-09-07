//! Approvals and permissions popup methods.
use crate::app_event::UiCommand;
use chaos_ipc::product::OS_NAME;
use chaos_ipc::protocol::PermissionGrantUpdate;
use chaos_ipc::protocol::PermissionUpdateScope;

use super::super::*;

impl ChatWidget {
    /// Open the permissions popup (alias for /permissions).
    pub fn open_approvals_popup(&mut self) {
        self.open_permissions_popup();
    }

    /// Open a popup to choose the permissions mode (approval policy + sandbox policy).
    pub fn open_permissions_popup(&mut self) {
        let mut items: Vec<SelectionItem> = Vec::new();

        for preset in Self::permission_presets() {
            let disabled_reason = self
                .config
                .permissions
                .approval_policy
                .can_set(&preset.approval)
                .err()
                .map(|err| err.to_string());
            let actions: Vec<SelectionAction> =
                if self.permission_preset_requires_confirmation(&preset) {
                    let preset_clone = preset.clone();
                    vec![Box::new(move |tx| {
                        tx.send(AppEvent::OpenFullAccessConfirmation {
                            preset: preset_clone.clone(),
                            return_to_permissions: true,
                        });
                    })]
                } else {
                    Self::permission_preset_actions(&preset)
                };
            items.push(SelectionItem {
                name: preset.label.to_string(),
                description: Some(preset.description.replace(" (Identical to Agent mode)", "")),
                is_current: self.permission_preset_is_current(&preset),
                actions,
                dismiss_on_select: true,
                disabled_reason,
                ..Default::default()
            });
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Update Model Permissions".to_string()),
            footer_note: None,
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(()),
            ..Default::default()
        });
    }

    /// Cycle through the same visible presets as /permissions, skipping disallowed choices.
    pub(crate) fn cycle_permissions(&mut self) {
        let presets = Self::permission_presets();
        let next_index = presets
            .iter()
            .position(|preset| self.permission_preset_is_current(preset))
            .map_or(0, |index| index + 1);
        let next = presets
            .iter()
            .cycle()
            .skip(next_index)
            .take(presets.len())
            .find(|preset| {
                !self.permission_preset_is_current(preset)
                    && self
                        .config
                        .permissions
                        .approval_policy
                        .can_set(&preset.approval)
                        .is_ok()
                    && self
                        .config
                        .permissions
                        .sandbox_policy
                        .can_set(&preset.sandbox)
                        .is_ok()
            })
            .cloned();
        let Some(preset) = next else {
            return;
        };

        if self.permission_preset_requires_confirmation(&preset) {
            // Cancelling a keyboard cycle returns to the draft, not a permissions picker.
            self.show_full_access_confirmation(preset, None);
        } else {
            for action in Self::permission_preset_actions(&preset) {
                action(&self.app_event_tx);
            }
        }
    }

    /// Keep the picker and keyboard cycle in the same order, excluding hidden presets.
    fn permission_presets() -> Vec<ApprovalPreset> {
        builtin_approval_presets()
            .into_iter()
            .filter(|preset| preset.id != "read-only")
            .collect()
    }

    fn permission_preset_is_current(&self, preset: &ApprovalPreset) -> bool {
        Self::preset_matches_current(
            self.config.permissions.approval_policy.value(),
            self.config.permissions.sandbox_policy.get(),
            preset,
        )
    }

    fn permission_preset_actions(preset: &ApprovalPreset) -> Vec<SelectionAction> {
        Self::approval_preset_actions(
            preset.approval,
            preset.sandbox.clone(),
            preset.label.to_string(),
            ApprovalsReviewer::User,
        )
    }

    fn full_access_approval_actions(
        preset: &ApprovalPreset,
        remember: bool,
    ) -> Vec<SelectionAction> {
        let mut actions = Self::permission_preset_actions(preset);
        actions.push(Box::new(move |tx| {
            tx.send(AppEvent::UpdateFullAccessWarningAcknowledged(true));
            if remember {
                tx.send(AppEvent::PersistFullAccessWarningAcknowledged);
            }
        }));
        actions
    }

    fn permission_preset_requires_confirmation(&self, preset: &ApprovalPreset) -> bool {
        preset.id == "full-access"
            && !self
                .config
                .notices
                .hide_full_access_warning
                .unwrap_or(false)
    }

    pub(crate) fn approval_preset_actions(
        approval: ApprovalPolicy,
        sandbox: SandboxPolicy,
        label: String,
        approvals_reviewer: ApprovalsReviewer,
    ) -> Vec<SelectionAction> {
        vec![Box::new(move |tx| {
            let sandbox_clone = sandbox.clone();
            // Unlike a turn-context override, this also updates the running
            // turn before its next tool call or retry.
            tx.send(AppEvent::ChaosOp(Op::UpdatePermissions {
                scope: PermissionUpdateScope::Session,
                expected_revision: None,
                approval_policy: Some(approval),
                sandbox_policy: Some(sandbox_clone.clone()),
                grants: PermissionGrantUpdate::Unchanged,
            }));
            tx.send(AppEvent::UpdateApprovalPolicy(approval));
            tx.send(AppEvent::UpdateSandboxPolicy(sandbox_clone));
            tx.send(AppEvent::UpdateApprovalsReviewer(approvals_reviewer));
            tx.emit_ui_command(UiCommand::Refresh);
            tx.send(AppEvent::InsertHistoryCell(Box::new(
                history_cell::new_info_event(
                    format!("Permissions updated to {label}"),
                    /*hint*/ None,
                ),
            )));
        })]
    }

    pub(crate) fn preset_matches_current(
        current_approval: ApprovalPolicy,
        current_sandbox: &SandboxPolicy,
        preset: &ApprovalPreset,
    ) -> bool {
        if current_approval != preset.approval {
            return false;
        }

        match (current_sandbox, &preset.sandbox) {
            (SandboxPolicy::RootAccess, SandboxPolicy::RootAccess) => true,
            (
                SandboxPolicy::ReadOnly {
                    network_access: current_network_access,
                    ..
                },
                SandboxPolicy::ReadOnly {
                    network_access: preset_network_access,
                    ..
                },
            )
            | (
                SandboxPolicy::WorkspaceWrite {
                    network_access: current_network_access,
                    ..
                },
                SandboxPolicy::WorkspaceWrite {
                    network_access: preset_network_access,
                    ..
                },
            ) => current_network_access == preset_network_access,
            _ => false,
        }
    }

    pub fn open_full_access_confirmation(
        &mut self,
        preset: ApprovalPreset,
        return_to_permissions: bool,
    ) {
        self.show_full_access_confirmation(preset, Some(return_to_permissions));
    }

    fn show_full_access_confirmation(
        &mut self,
        preset: ApprovalPreset,
        return_to_permissions: Option<bool>,
    ) {
        let mut header_children: Vec<Box<dyn Renderable>> = Vec::new();
        let title_line = Line::from("Enable full access?").bold();
        let info_line = Line::from(vec![
            format!("When {OS_NAME} runs with full access, it can edit any file on your computer and run commands with network, without your approval. ")
                .into(),
            "Exercise caution when enabling full access. This significantly increases the risk of data loss, leaks, or unexpected behavior."
                .fg(crate::theme::error_color()),
        ]);
        header_children.push(Box::new(title_line));
        header_children.push(Box::new(
            Paragraph::new(vec![info_line]).wrap(Wrap { trim: false }),
        ));
        let header = ColumnRenderable::with(header_children);

        let deny_actions: Vec<SelectionAction> =
            vec![Box::new(move |tx| match return_to_permissions {
                Some(true) => tx.send(AppEvent::OpenPermissionsPopup),
                Some(false) => tx.send(AppEvent::OpenApprovalsPopup),
                None => {}
            })];

        let items = vec![
            SelectionItem {
                name: "Yes, continue anyway".to_string(),
                description: Some("Apply full access for this session".to_string()),
                actions: Self::full_access_approval_actions(&preset, false),
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Yes, and don't ask again".to_string(),
                description: Some("Enable full access and remember this choice".to_string()),
                actions: Self::full_access_approval_actions(&preset, true),
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Cancel".to_string(),
                description: Some("Go back without enabling full access".to_string()),
                actions: deny_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some(standard_popup_hint_line()),
            items,
            header: Box::new(header),
            ..Default::default()
        });
    }
}
