use std::sync::Arc;

use chaos_ipc::protocol::ChaosErrorInfo;
use chaos_ipc::protocol::ErrorEvent;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use tracing::warn;

use crate::SandboxState;
use crate::chaos::SessionConfiguration;
use crate::chaos::SessionSettingsUpdate;
use crate::chaos::TurnContext;
use crate::chaos::turn_context::make_turn_context;
use crate::config::StartedNetworkProxy;
use crate::prompt_images::response_input_item_from_user_input;

use super::Session;

impl Session {
    pub(crate) fn subscribe_out_of_band_elicitation_pause_state(
        &self,
    ) -> tokio::sync::watch::Receiver<bool> {
        self.out_of_band_elicitation_paused.subscribe()
    }

    pub(crate) fn set_out_of_band_elicitation_pause_state(&self, paused: bool) {
        self.out_of_band_elicitation_paused.send_replace(paused);
    }

    pub(super) fn next_internal_sub_id(&self) -> String {
        let id = self
            .next_internal_sub_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("auto-compact-{id}")
    }

    pub(crate) async fn new_turn_with_sub_id(
        &self,
        sub_id: String,
        updates: SessionSettingsUpdate,
    ) -> crate::config::ConstraintResult<Arc<TurnContext>> {
        let (
            session_configuration,
            sandbox_policy_changed,
            previous_cwd,
            chaos_home,
            session_source,
        ) = {
            let mut state = self.state.lock().await;
            match state.session_configuration.clone().apply(&updates) {
                Ok(next) => {
                    let previous_cwd = state.session_configuration.cwd.clone();
                    let sandbox_policy_changed = state.session_configuration.vfs_policy
                        != next.vfs_policy
                        || state.session_configuration.socket_policy != next.socket_policy;
                    let chaos_home = next.chaos_home.clone();
                    let session_source = next.session_source.clone();
                    state.session_configuration = next.clone();
                    (
                        next,
                        sandbox_policy_changed,
                        previous_cwd,
                        chaos_home,
                        session_source,
                    )
                }
                Err(err) => {
                    drop(state);
                    self.send_event_raw(Event {
                        id: sub_id.clone(),
                        msg: EventMsg::Error(ErrorEvent {
                            message: err.to_string(),
                            chaos_error_info: Some(ChaosErrorInfo::BadRequest),
                        }),
                    })
                    .await;
                    return Err(err);
                }
            }
        };

        self.maybe_refresh_shell_snapshot_for_cwd(
            &previous_cwd,
            &session_configuration.cwd,
            &chaos_home,
            &session_source,
        );

        if previous_cwd != session_configuration.cwd
            && let Err(e) = self
                .services
                .mcp_connection_manager
                .read()
                .await
                .notify_roots_changed(&session_configuration.cwd)
                .await
        {
            warn!("Failed to notify MCP servers of roots change: {e:#}");
        }

        Ok(self
            .new_turn_from_configuration(
                sub_id,
                session_configuration,
                updates.final_output_json_schema,
                sandbox_policy_changed,
            )
            .await)
    }

    pub(crate) async fn new_turn_from_configuration(
        &self,
        sub_id: String,
        session_configuration: SessionConfiguration,
        final_output_json_schema: Option<Option<serde_json::Value>>,
        sandbox_policy_changed: bool,
    ) -> Arc<TurnContext> {
        let per_turn_config = Self::build_per_turn_config(&session_configuration);
        self.services
            .mcp_connection_manager
            .read()
            .await
            .set_approval_policy(&session_configuration.approval_policy);

        if sandbox_policy_changed {
            let sandbox_state = SandboxState {
                vfs_policy: session_configuration.vfs_policy.clone(),
                socket_policy: session_configuration.socket_policy,
                alcatraz_macos_exe: per_turn_config.alcatraz_macos_exe.clone(),
                alcatraz_linux_exe: per_turn_config.alcatraz_linux_exe.clone(),
                alcatraz_freebsd_exe: per_turn_config.alcatraz_freebsd_exe.clone(),
                sandbox_cwd: per_turn_config.cwd.clone(),
            };
            if let Err(e) = self
                .services
                .mcp_connection_manager
                .read()
                .await
                .notify_sandbox_state_change(&sandbox_state)
                .await
            {
                warn!("Failed to notify sandbox state change to MCP servers: {e:#}");
            }
        }

        let model_info = self
            .services
            .models_manager
            .get_model_info(
                session_configuration.collaboration_mode.model(),
                &per_turn_config,
            )
            .await;
        let mut turn_context: TurnContext = make_turn_context(
            Some(Arc::clone(&self.services.auth_manager)),
            &self.services.session_telemetry,
            session_configuration.provider.clone(),
            &session_configuration,
            per_turn_config,
            model_info,
            &self.services.models_manager,
            self.services
                .network_proxy
                .as_ref()
                .map(StartedNetworkProxy::proxy),
            sub_id,
        );

        if let Some(final_schema) = final_output_json_schema {
            turn_context.final_output_json_schema = final_schema;
        }
        let turn_context = Arc::new(turn_context);
        turn_context.turn_metadata_state.spawn_git_enrichment_task();
        turn_context
    }

    pub(crate) async fn new_default_turn(&self) -> Arc<TurnContext> {
        self.new_default_turn_with_sub_id(self.next_internal_sub_id())
            .await
    }

    pub(crate) async fn new_default_turn_with_sub_id(&self, sub_id: String) -> Arc<TurnContext> {
        let session_configuration = {
            let state = self.state.lock().await;
            state.session_configuration.clone()
        };
        self.new_turn_from_configuration(
            sub_id,
            session_configuration,
            /*final_output_json_schema*/ None,
            /*sandbox_policy_changed*/ false,
        )
        .await
    }

    /// Inject additional user input into the currently active turn.
    pub async fn steer_input(
        &self,
        input: Vec<chaos_ipc::user_input::UserInput>,
        expected_turn_id: Option<&str>,
    ) -> Result<String, crate::chaos::SteerInputError> {
        use crate::chaos::SteerInputError;
        if input.is_empty() {
            return Err(SteerInputError::EmptyInput);
        }

        let mut active = self.active_turn.lock().await;
        let Some(active_turn) = active.as_mut() else {
            return Err(SteerInputError::NoActiveTurn(input));
        };

        let Some((active_turn_id, _)) = active_turn.tasks.first() else {
            return Err(SteerInputError::NoActiveTurn(input));
        };

        if let Some(expected_turn_id) = expected_turn_id
            && expected_turn_id != active_turn_id
        {
            return Err(SteerInputError::ExpectedTurnMismatch {
                expected: expected_turn_id.to_string(),
                actual: active_turn_id.clone(),
            });
        }

        let mut turn_state = active_turn.turn_state.lock().await;
        turn_state.push_pending_input(response_input_item_from_user_input(input));
        Ok(active_turn_id.clone())
    }

    pub async fn inject_response_items(
        &self,
        input: Vec<chaos_ipc::models::ResponseInputItem>,
    ) -> Result<(), Vec<chaos_ipc::models::ResponseInputItem>> {
        let mut active = self.active_turn.lock().await;
        match active.as_mut() {
            Some(at) => {
                let mut ts = at.turn_state.lock().await;
                for item in input {
                    ts.push_pending_input(item);
                }
                Ok(())
            }
            None => Err(input),
        }
    }

    pub async fn get_pending_input(&self) -> Vec<chaos_ipc::models::ResponseInputItem> {
        let mut active = self.active_turn.lock().await;
        match active.as_mut() {
            Some(at) => {
                let mut ts = at.turn_state.lock().await;
                ts.take_pending_input()
            }
            None => Vec::with_capacity(0),
        }
    }

    pub async fn has_deliverable_input(&self) -> bool {
        let active = self.active_turn.lock().await;
        match active.as_ref() {
            Some(at) => {
                let ts = at.turn_state.lock().await;
                ts.has_deliverable_input()
            }
            None => false,
        }
    }

    pub(crate) async fn turn_context_for_sub_id(&self, sub_id: &str) -> Option<Arc<TurnContext>> {
        let active = self.active_turn.lock().await;
        active
            .as_ref()
            .and_then(|turn| turn.tasks.get(sub_id))
            .map(|task| Arc::clone(&task.turn_context))
    }

    pub(crate) async fn active_turn_context_and_cancellation_token(
        &self,
    ) -> Option<(Arc<TurnContext>, tokio_util::sync::CancellationToken)> {
        let active = self.active_turn.lock().await;
        let (_, task) = active.as_ref()?.tasks.first()?;
        Some((
            Arc::clone(&task.turn_context),
            task.cancellation_token.child_token(),
        ))
    }

    pub(super) async fn turn_state_for_sub_id(
        &self,
        sub_id: &str,
    ) -> Option<Arc<tokio::sync::Mutex<crate::state::TurnState>>> {
        let active = self.active_turn.lock().await;
        active.as_ref().and_then(|at| {
            at.tasks
                .contains_key(sub_id)
                .then(|| Arc::clone(&at.turn_state))
        })
    }

    pub async fn defer_mailbox_delivery_to_next_turn(&self, sub_id: &str) {
        let Some(turn_state) = self.turn_state_for_sub_id(sub_id).await else {
            return;
        };
        let mut ts = turn_state.lock().await;
        ts.record_answer_emitted();
    }

    pub async fn accept_mailbox_delivery_for_current_turn(&self, sub_id: &str) {
        let Some(turn_state) = self.turn_state_for_sub_id(sub_id).await else {
            return;
        };
        let mut ts = turn_state.lock().await;
        ts.record_tool_call_emitted();
    }

    pub async fn interrupt_task(self: &Arc<Self>) {
        use tracing::info;
        info!("interrupt received: abort current task, if any");
        let has_active_turn = { self.active_turn.lock().await.is_some() };
        if has_active_turn {
            self.abort_all_tasks(chaos_ipc::protocol::TurnAbortReason::Interrupted)
                .await;
        } else {
            self.cancel_mcp_startup().await;
        }
    }

    pub async fn notify_dynamic_tool_response(
        &self,
        call_id: &str,
        response: chaos_ipc::dynamic_tools::DynamicToolResponse,
    ) {
        use tracing::warn;
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_dynamic_tool(call_id)
                }
                None => None,
            }
        };
        match entry {
            Some(tx_response) => {
                tx_response.send(response).ok();
            }
            None => {
                warn!("No pending dynamic tool call found for call_id: {call_id}");
            }
        }
    }

    pub(crate) async fn maybe_start_ghost_snapshot(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) {
        use crate::tasks::GhostSnapshotTask;
        use crate::tasks::SessionTask;
        use crate::tasks::SessionTaskContext;
        use chaos_ready::Readiness;
        use tracing::info;
        use tracing::warn;

        let token = match turn_context.tool_call_gate.subscribe().await {
            Ok(token) => token,
            Err(err) => {
                warn!("failed to subscribe to ghost snapshot readiness: {err}");
                return;
            }
        };

        info!("spawning ghost snapshot task");
        let task = GhostSnapshotTask::new(token);
        Arc::new(task)
            .run(
                Arc::new(SessionTaskContext::new(self.clone())),
                turn_context.clone(),
                Vec::new(),
                cancellation_token,
            )
            .await;
    }
}
