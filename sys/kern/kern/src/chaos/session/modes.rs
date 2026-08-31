use std::sync::Arc;

use chaos_ipc::models::DeveloperInstructions;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::SessionModeChangedEvent;
use chaos_ipc::protocol::SessionSource;
use serde::Serialize;

use super::Session;
use crate::chaos::TurnContext;
use crate::config::AgentCompactionControl;
use crate::config::Constrained;
use crate::config::TerminalTitleMode;
use crate::modes::ModePolicy;
use crate::tools::spec::ToolsConfig;
use crate::tools::spec::ToolsConfigParams;

#[derive(Debug, Serialize)]
pub(crate) struct ModeSwitchResult {
    pub(crate) previous_mode: String,
    pub(crate) active_mode: String,
    pub(crate) changed: bool,
}

impl Session {
    pub(crate) async fn modes_json(&self) -> Result<String, String> {
        let state = self.state.lock().await;
        state
            .session_configuration
            .mode_registry
            .resource_json(&state.session_configuration.mode_policy)
    }

    pub(crate) async fn child_mode_policy(
        &self,
        turn_context: &TurnContext,
        requested_mode: Option<&str>,
        requested_allowed_modes: Option<&[String]>,
        requested_switching: Option<bool>,
    ) -> Result<ModePolicy, String> {
        let state = self.state.lock().await;
        turn_context.mode_policy.child(
            &state.session_configuration.mode_registry,
            turn_context.mode_capabilities,
            requested_mode,
            requested_allowed_modes,
            requested_switching,
        )
    }

    pub(crate) async fn switch_mode(
        &self,
        mode_id: &str,
        turn_context: &TurnContext,
    ) -> Result<ModeSwitchResult, String> {
        let mode_id = mode_id.trim();
        let (previous_mode, collaboration_mode, mode_title, sync_main_ui) = {
            let mut state = self.state.lock().await;
            let session_configuration = &mut state.session_configuration;
            if !session_configuration.mode_policy.switching_allowed {
                return Err("mode switching is disabled for this session".to_string());
            }
            if !session_configuration
                .mode_policy
                .allowed_modes
                .contains(mode_id)
            {
                return Err(format!(
                    "mode `{mode_id}` is not allowed for this session; read chaos://modes for the caller-filtered catalog"
                ));
            }
            let previous_mode = session_configuration.mode_policy.active_mode.clone();
            if previous_mode == mode_id {
                return Ok(ModeSwitchResult {
                    previous_mode: previous_mode.clone(),
                    active_mode: previous_mode,
                    changed: false,
                });
            }
            let mode_title = session_configuration
                .mode_registry
                .get(mode_id)
                .unwrap_or_else(|| panic!("allowed mode must be registered"))
                .title
                .clone();
            let collaboration_mode = session_configuration.mode_registry.apply_mode(
                mode_id,
                &session_configuration.collaboration_mode.with_updates(
                    /*model*/ None,
                    Some(session_configuration.mode_base_reasoning_effort),
                    /*minion_instructions*/ None,
                ),
            )?;
            session_configuration.mode_policy.active_mode = mode_id.to_string();
            session_configuration.collaboration_mode = collaboration_mode.clone();
            let sync_main_ui = !matches!(
                &session_configuration.session_source,
                SessionSource::SubAgent(_)
            );
            (previous_mode, collaboration_mode, mode_title, sync_main_ui)
        };

        let transition = DeveloperInstructions::new(format!(
            "<mode_switch>\nChaOS switched this session from `{previous_mode}` to `{mode_id}`. The following collaboration mode instructions are authoritative for subsequent samples, including the next sample in this user turn.\n</mode_switch>"
        ));
        let instructions = DeveloperInstructions::from_collaboration_mode(&collaboration_mode)
            .map_or(transition.clone(), |mode| transition.concat(mode));
        let item: ResponseItem = instructions.into();
        self.record_conversation_items(turn_context, std::slice::from_ref(&item))
            .await;
        if sync_main_ui {
            self.send_transient_event(
                turn_context,
                EventMsg::SessionModeChanged(SessionModeChangedEvent {
                    session_id: self.conversation_id,
                    mode_id: mode_id.to_string(),
                    mode_title,
                    mode_kind: collaboration_mode.mode,
                    model: collaboration_mode.model().to_string(),
                    reasoning_effort: collaboration_mode.reasoning_effort(),
                }),
            )
            .await;
        }

        Ok(ModeSwitchResult {
            previous_mode,
            active_mode: mode_id.to_string(),
            changed: true,
        })
    }

    pub(crate) async fn effective_turn_context(&self, base: &Arc<TurnContext>) -> Arc<TurnContext> {
        let (collaboration_mode, mode_registry, mode_policy) = {
            let state = self.state.lock().await;
            (
                state.session_configuration.collaboration_mode.clone(),
                Arc::clone(&state.session_configuration.mode_registry),
                state.session_configuration.mode_policy.clone(),
            )
        };
        let permission_snapshot = self.permission_snapshot(base).await;
        let effective_vfs_policy = permission_snapshot.effective_vfs_policy();
        let effective_socket_policy = permission_snapshot.effective_socket_policy();
        let permissions_unchanged = base.approval_policy.value()
            == permission_snapshot.approval_policy
            && base.vfs_policy == effective_vfs_policy
            && base.socket_policy == effective_socket_policy;
        if base.mode_id == mode_policy.active_mode
            && base.mode_policy == mode_policy
            && base.collaboration_mode == collaboration_mode
            && permissions_unchanged
        {
            return Arc::clone(base);
        }

        let mode_capabilities = mode_registry
            .get(&mode_policy.active_mode)
            .unwrap_or_else(|| {
                panic!("validated session mode policy must reference a registered mode");
            })
            .capabilities;
        let mut config = (*base.config).clone();
        config.model_reasoning_effort = collaboration_mode.reasoning_effort();
        config.mode_policy_override = Some(mode_policy.clone());
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &base.model_info,
            available_models: &self
                .services
                .models_manager
                .try_list_models()
                .unwrap_or_default(),
            approval_policy: permission_snapshot.approval_policy,
            minion_jobs_allowed: config.minion_jobs_allowed,
            web_search_mode: base.tools_config.web_search_mode,
            session_source: base.session_source.clone(),
            vfs_policy: &effective_vfs_policy,
            collab_enabled: config.collab_enabled,
        })
        .with_agent_compaction_control(matches!(
            config.agent_compaction_control,
            AgentCompactionControl::Bounded
        ))
        .with_agent_session_title(
            matches!(config.terminal_title, TerminalTitleMode::Agent),
            &base.session_source,
        )
        .with_dynamic_parent_effort(config.dynamic_parent_effort, &base.session_source)
        .with_unified_exec_shell_mode(base.tools_config.unified_exec_shell_mode.clone())
        .with_web_search_config(base.tools_config.web_search_config.clone())
        .with_allow_login_shell(base.tools_config.allow_login_shell)
        .with_agent_roles(config.agent_roles.clone())
        .with_mode_policy(mode_capabilities, mode_policy.switching_allowed);

        let mut effective = (**base).clone();
        effective.config = Arc::new(config);
        effective.reasoning_effort = collaboration_mode.reasoning_effort();
        effective.collaboration_mode = collaboration_mode;
        effective.mode_id = mode_policy.active_mode.clone();
        effective.mode_capabilities = mode_capabilities;
        effective.mode_policy = mode_policy;
        if effective
            .approval_policy
            .set(permission_snapshot.approval_policy)
            .is_err()
        {
            effective.approval_policy = Constrained::allow_any(permission_snapshot.approval_policy);
        }
        effective.vfs_policy = effective_vfs_policy;
        effective.socket_policy = effective_socket_policy;
        effective.tools_config = tools_config;
        Arc::new(effective)
    }
}
