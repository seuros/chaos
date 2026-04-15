use std::collections::HashMap;
use std::path::PathBuf;

use chaos_ipc::approvals::ExecPolicyAmendment;
use chaos_ipc::approvals::NetworkPolicyAmendment;
use chaos_ipc::approvals::NetworkPolicyRuleAction;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::DeveloperInstructions;
use chaos_ipc::models::PermissionProfile;
use chaos_ipc::models::ResponseInputItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::models::format_allow_prefixes;
use chaos_ipc::protocol::ApplyPatchApprovalRequestEvent;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::ExecApprovalRequestEvent;
use chaos_ipc::protocol::FileChange;
use chaos_ipc::protocol::NetworkApprovalContext;
use chaos_ipc::protocol::RequestUserInputEvent;
use chaos_ipc::protocol::ReviewDecision;
use chaos_ipc::request_permissions::PermissionGrantScope;
use chaos_ipc::request_permissions::RequestPermissionProfile;
use chaos_ipc::request_permissions::RequestPermissionsArgs;
use chaos_ipc::request_permissions::RequestPermissionsEvent;
use chaos_ipc::request_permissions::RequestPermissionsResponse;
use chaos_ipc::request_user_input::RequestUserInputArgs;
use chaos_ipc::request_user_input::RequestUserInputResponse;
use chaos_pf::normalize_host;
use tokio::sync::oneshot;
use tracing::error;
use tracing::warn;

use super::Session;
use super::TurnContext;
use crate::exec_policy::ExecPolicyUpdateError;
use crate::network_policy_decision::execpolicy_network_rule_amendment;
use crate::protocol::ApprovalPolicy;
use crate::state::turn::ApprovalKind;
use crate::state::turn::PendingInsert;

impl Session {
    /// Adds an execpolicy amendment to both the in-memory and on-disk policies
    /// so future commands can use the newly approved prefix.
    pub(crate) async fn persist_execpolicy_amendment(
        &self,
        amendment: &ExecPolicyAmendment,
    ) -> Result<(), ExecPolicyUpdateError> {
        let chaos_home = self
            .state
            .lock()
            .await
            .session_configuration
            .chaos_home()
            .clone();

        self.services
            .exec_policy
            .append_amendment_and_update(&chaos_home, amendment)
            .await?;

        Ok(())
    }

    pub(crate) async fn record_execpolicy_amendment_message(
        &self,
        sub_id: &str,
        amendment: &ExecPolicyAmendment,
    ) {
        let Some(prefixes) = format_allow_prefixes(vec![amendment.command.clone()]) else {
            warn!("execpolicy amendment for {sub_id} had no command prefix");
            return;
        };
        let text = format!("Approved command prefix saved:\n{prefixes}");
        let message: ResponseItem = DeveloperInstructions::new(text.clone()).into();

        if let Some(turn_context) = self.turn_context_for_sub_id(sub_id).await {
            self.record_conversation_items(&turn_context, std::slice::from_ref(&message))
                .await;
            return;
        }

        if self
            .inject_response_items(vec![ResponseInputItem::Message {
                role: "system".to_string(),
                content: vec![ContentItem::InputText { text }],
            }])
            .await
            .is_err()
        {
            warn!("no active turn found to record execpolicy amendment message for {sub_id}");
        }
    }

    pub(crate) async fn persist_network_policy_amendment(
        &self,
        amendment: &NetworkPolicyAmendment,
        network_approval_context: &NetworkApprovalContext,
    ) -> anyhow::Result<()> {
        let host =
            Self::validated_network_policy_amendment_host(amendment, network_approval_context)?;
        let chaos_home = self
            .state
            .lock()
            .await
            .session_configuration
            .chaos_home()
            .clone();
        let execpolicy_amendment =
            execpolicy_network_rule_amendment(amendment, network_approval_context, &host);

        if let Some(started_network_proxy) = self.services.network_proxy.as_ref() {
            let proxy = started_network_proxy.proxy();
            match amendment.action {
                NetworkPolicyRuleAction::Allow => proxy
                    .add_allowed_domain(&host)
                    .await
                    .map_err(|err| anyhow::anyhow!("failed to update runtime allowlist: {err}"))?,
                NetworkPolicyRuleAction::Deny => proxy
                    .add_denied_domain(&host)
                    .await
                    .map_err(|err| anyhow::anyhow!("failed to update runtime denylist: {err}"))?,
            }
        }

        self.services
            .exec_policy
            .append_network_rule_and_update(
                &chaos_home,
                &host,
                execpolicy_amendment.protocol,
                execpolicy_amendment.decision,
                Some(execpolicy_amendment.justification),
            )
            .await
            .map_err(|err| {
                anyhow::anyhow!("failed to persist network policy amendment to execpolicy: {err}")
            })?;

        Ok(())
    }

    pub(crate) fn validated_network_policy_amendment_host(
        amendment: &NetworkPolicyAmendment,
        network_approval_context: &NetworkApprovalContext,
    ) -> anyhow::Result<String> {
        let approved_host = normalize_host(&network_approval_context.host);
        let amendment_host = normalize_host(&amendment.host);
        if amendment_host != approved_host {
            return Err(anyhow::anyhow!(
                "network policy amendment host '{}' does not match approved host '{}'",
                amendment.host,
                network_approval_context.host
            ));
        }
        Ok(approved_host)
    }

    pub(crate) async fn record_network_policy_amendment_message(
        &self,
        sub_id: &str,
        amendment: &NetworkPolicyAmendment,
    ) {
        let (action, list_name) = match amendment.action {
            NetworkPolicyRuleAction::Allow => ("Allowed", "allowlist"),
            NetworkPolicyRuleAction::Deny => ("Denied", "denylist"),
        };
        let text = format!(
            "{action} network rule saved in execpolicy ({list_name}): {}",
            amendment.host
        );
        let message: ResponseItem = DeveloperInstructions::new(text.clone()).into();

        if let Some(turn_context) = self.turn_context_for_sub_id(sub_id).await {
            self.record_conversation_items(&turn_context, std::slice::from_ref(&message))
                .await;
            return;
        }

        if self
            .inject_response_items(vec![ResponseInputItem::Message {
                role: "system".to_string(),
                content: vec![ContentItem::InputText { text }],
            }])
            .await
            .is_err()
        {
            warn!(
                "no active turn found to record network policy amendment \
                 message for {sub_id}"
            );
        }
    }

    /// Emit an exec approval request event and await the user's decision.
    ///
    /// The request is keyed by `call_id` + `approval_id` so matching responses
    /// are delivered to the correct in-flight turn. If the pending approval is
    /// cleared before a response arrives, treat it as an abort so interrupted
    /// turns do not continue on a synthetic denial.
    ///
    /// Note that if `available_decisions` is `None`, then the other fields
    /// will be used to derive the available decisions via
    /// [ExecApprovalRequestEvent::default_available_decisions].
    #[allow(clippy::too_many_arguments)]
    pub async fn request_command_approval(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        approval_id: Option<String>,
        command: Vec<String>,
        cwd: PathBuf,
        reason: Option<String>,
        network_approval_context: Option<NetworkApprovalContext>,
        proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
        additional_permissions: Option<PermissionProfile>,
        available_decisions: Option<Vec<ReviewDecision>>,
    ) -> ReviewDecision {
        // Command-level approvals use `call_id`.
        // `approval_id` is only present for subcommand callbacks (execve intercept).
        let effective_approval_id = approval_id.clone().unwrap_or_else(|| call_id.clone());
        // Add the tx_approve callback to the map before sending the request.
        let (tx_approve, rx_approve) = oneshot::channel();
        let insert_result = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_approval(
                        ApprovalKind::Exec,
                        effective_approval_id.clone(),
                        tx_approve,
                    )
                }
                None => PendingInsert::Inserted, // no active turn: send-only best effort
            }
        };
        if let PendingInsert::Duplicate(_) = insert_result {
            error!(
                "Duplicate pending exec approval for call_id: \
                 {effective_approval_id}; aborting the new request to preserve \
                 the in-flight responder"
            );
            return ReviewDecision::Abort;
        }

        let parsed_cmd = crate::parse_command::parse_command(&command);
        let proposed_network_policy_amendments = network_approval_context.as_ref().map(|context| {
            vec![
                NetworkPolicyAmendment {
                    host: context.host.clone(),
                    action: NetworkPolicyRuleAction::Allow,
                },
                NetworkPolicyAmendment {
                    host: context.host.clone(),
                    action: NetworkPolicyRuleAction::Deny,
                },
            ]
        });
        let available_decisions = available_decisions.unwrap_or_else(|| {
            ExecApprovalRequestEvent::default_available_decisions(
                network_approval_context.as_ref(),
                proposed_execpolicy_amendment.as_ref(),
                proposed_network_policy_amendments.as_deref(),
                additional_permissions.as_ref(),
            )
        });
        let event = EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
            call_id,
            approval_id,
            turn_id: turn_context.sub_id.clone(),
            command,
            cwd,
            reason,
            network_approval_context,
            proposed_execpolicy_amendment,
            proposed_network_policy_amendments,
            additional_permissions,
            available_decisions: Some(available_decisions),
            parsed_cmd,
        });
        self.send_event(turn_context, event).await;
        rx_approve.await.unwrap_or(ReviewDecision::Abort)
    }

    pub async fn request_patch_approval(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        changes: HashMap<PathBuf, FileChange>,
        reason: Option<String>,
        grant_root: Option<PathBuf>,
    ) -> oneshot::Receiver<ReviewDecision> {
        // Add the tx_approve callback to the map before sending the request.
        let (tx_approve, rx_approve) = oneshot::channel();
        let approval_id = call_id.clone();
        let insert_result = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_approval(ApprovalKind::Patch, approval_id.clone(), tx_approve)
                }
                None => PendingInsert::Inserted,
            }
        };
        if let PendingInsert::Duplicate(tx_reject) = insert_result {
            error!(
                "Duplicate pending patch approval for call_id: {approval_id}; \
                 aborting the new request to preserve the in-flight responder"
            );
            // Deliver an immediate abort through the caller's receiver so the
            // downstream `.await` resolves without racing the original request.
            let _ = tx_reject.send(ReviewDecision::Abort);
            return rx_approve;
        }

        let event = EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
            call_id,
            turn_id: turn_context.sub_id.clone(),
            changes,
            reason,
            grant_root,
        });
        self.send_event(turn_context, event).await;
        rx_approve
    }

    pub async fn request_permissions(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        args: RequestPermissionsArgs,
    ) -> Option<RequestPermissionsResponse> {
        match turn_context.approval_policy.value() {
            ApprovalPolicy::Headless => {
                return Some(RequestPermissionsResponse {
                    permissions: RequestPermissionProfile::default(),
                    scope: PermissionGrantScope::Turn,
                });
            }
            ApprovalPolicy::Granular(granular_config)
                if !granular_config.allows_request_permissions() =>
            {
                return Some(RequestPermissionsResponse {
                    permissions: RequestPermissionProfile::default(),
                    scope: PermissionGrantScope::Turn,
                });
            }
            ApprovalPolicy::Interactive
            | ApprovalPolicy::Supervised
            | ApprovalPolicy::Granular(_) => {}
        }

        let (tx_response, rx_response) = oneshot::channel();
        let insert_result = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_request_permissions(call_id.clone(), tx_response)
                }
                None => PendingInsert::Inserted,
            }
        };
        if let PendingInsert::Duplicate(_) = insert_result {
            error!(
                "Duplicate pending request_permissions for call_id: {call_id}; \
                 returning default profile to preserve the in-flight responder"
            );
            return Some(RequestPermissionsResponse {
                permissions: RequestPermissionProfile::default(),
                scope: PermissionGrantScope::Turn,
            });
        }

        let event = EventMsg::RequestPermissions(RequestPermissionsEvent {
            call_id,
            turn_id: turn_context.sub_id.clone(),
            reason: args.reason,
            permissions: args.permissions,
        });
        self.send_event(turn_context, event).await;
        rx_response.await.ok()
    }

    pub async fn notify_user_input_response(
        &self,
        sub_id: &str,
        response: RequestUserInputResponse,
    ) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_user_input(sub_id)
                }
                None => None,
            }
        };
        match entry {
            Some(tx_response) => {
                tx_response.send(response).ok();
            }
            None => {
                warn!("No pending user input found for sub_id: {sub_id}");
            }
        }
    }

    pub async fn notify_request_permissions_response(
        &self,
        call_id: &str,
        response: RequestPermissionsResponse,
    ) {
        let mut granted_for_session = None;
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    let entry = ts.remove_pending_request_permissions(call_id);
                    if entry.is_some() && !response.permissions.is_empty() {
                        match response.scope {
                            PermissionGrantScope::Turn => {
                                ts.record_granted_permissions(response.permissions.clone().into());
                            }
                            PermissionGrantScope::Session => {
                                granted_for_session = Some(response.permissions.clone());
                            }
                        }
                    }
                    entry
                }
                None => None,
            }
        };
        if let Some(permissions) = granted_for_session {
            let mut state = self.state.lock().await;
            state.record_granted_permissions(permissions.into());
        }
        match entry {
            Some(tx_response) => {
                tx_response.send(response).ok();
            }
            None => {
                warn!("No pending request_permissions found for call_id: {call_id}");
            }
        }
    }

    /// Deliver a decision to a pending approval. `kind` selects the
    /// namespace so an exec `call_id` cannot resolve a patch approval (and
    /// vice-versa) even if the ids happen to collide.
    pub async fn notify_approval(
        &self,
        kind: ApprovalKind,
        approval_id: &str,
        decision: ReviewDecision,
    ) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_approval(kind, approval_id)
                }
                None => None,
            }
        };
        match entry {
            Some(tx_approve) => {
                tx_approve.send(decision).ok();
            }
            None => {
                warn!("No pending approval found for kind: {kind:?} call_id: {approval_id}");
            }
        }
    }

    /// Convenience wrapper: exec approvals are the common case.
    pub async fn notify_exec_approval(&self, approval_id: &str, decision: ReviewDecision) {
        self.notify_approval(ApprovalKind::Exec, approval_id, decision)
            .await;
    }

    /// Convenience wrapper for patch approvals.
    pub async fn notify_patch_approval(&self, approval_id: &str, decision: ReviewDecision) {
        self.notify_approval(ApprovalKind::Patch, approval_id, decision)
            .await;
    }

    pub(crate) async fn granted_turn_permissions(&self) -> Option<PermissionProfile> {
        let active = self.active_turn.lock().await;
        let active = active.as_ref()?;
        let ts = active.turn_state.lock().await;
        ts.granted_permissions()
    }

    pub(crate) async fn granted_session_permissions(&self) -> Option<PermissionProfile> {
        let state = self.state.lock().await;
        state.granted_permissions()
    }

    /// Emit a user input request and await the user's response.
    pub async fn request_user_input(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        args: RequestUserInputArgs,
    ) -> Option<RequestUserInputResponse> {
        let sub_id = turn_context.sub_id.clone();
        let (tx_response, rx_response) = oneshot::channel();
        let event_id = sub_id.clone();
        let insert_result = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_user_input(sub_id, tx_response)
                }
                None => PendingInsert::Inserted,
            }
        };
        if let PendingInsert::Duplicate(_) = insert_result {
            error!("Duplicate pending user input for sub_id: {event_id}");
            return None;
        }

        let event = EventMsg::RequestUserInput(RequestUserInputEvent {
            call_id,
            turn_id: turn_context.sub_id.clone(),
            questions: args.questions,
        });
        self.send_event(turn_context, event).await;
        rx_response.await.ok()
    }
}
