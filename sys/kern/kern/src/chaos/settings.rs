use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use chaos_ipc::api::SettingsScope;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::SubAgentSource;
use tracing::warn;

use super::Session;
use super::TurnContext;
use crate::config::Config;
use crate::config::ConstraintResult;

/// Convenience wrapper used by `new_turn_with_sub_id`.
pub(super) use super::SessionSettingsUpdate;

impl Session {
    pub(crate) async fn apply_configuration_update(
        &self,
        configuration: &super::SessionConfiguration,
        updates: &SessionSettingsUpdate,
    ) -> ConstraintResult<super::SessionConfiguration> {
        let mut next = configuration.apply(updates)?;
        self.resolve_model_configuration(&mut next).await?;
        Ok(next)
    }

    /// Normalize a picker identity at the native configuration boundary. Both
    /// frontends use this path; wire slugs never carry account information.
    pub(crate) async fn resolve_model_configuration(
        &self,
        configuration: &mut super::SessionConfiguration,
    ) -> ConstraintResult<()> {
        let requested = configuration.collaboration_mode.model();
        if !crate::models_manager::catalog::ModelSelection::is_id(requested) {
            return Ok(());
        }
        let mut config = Self::build_per_turn_config(configuration);
        config.model = Some(requested.to_string());
        self.services
            .models_manager
            .resolve_model_selection(&mut config)
            .await
            .map_err(|error| crate::config::ConstraintError::ModelSelection {
                reason: error.to_string(),
            })?;
        configuration.provider = config.model_provider.clone();
        configuration.collaboration_mode = configuration.collaboration_mode.with_updates(
            config.model.clone(),
            Some(config.model_reasoning_effort),
            None,
        );
        configuration.mode_base_reasoning_effort = config.model_reasoning_effort;
        configuration.original_config_do_not_use = Arc::new(config);
        Ok(())
    }

    pub(crate) async fn chaos_home(&self) -> PathBuf {
        let state = self.state.lock().await;
        state.session_configuration.chaos_home().clone()
    }

    /// Refresh the captured shell environment when the working directory changes, unless
    /// this session is a sub-agent process spawn (which inherits from parent).
    pub(super) async fn maybe_refresh_shell_environment_for_cwd(
        &self,
        previous_cwd: &Path,
        next_cwd: &Path,
        session_source: &SessionSource,
    ) {
        if previous_cwd == next_cwd {
            return;
        }

        if matches!(
            session_source,
            SessionSource::SubAgent(SubAgentSource::ProcessSpawn { .. })
        ) {
            return;
        }

        self.services
            .shell_environment
            .refresh(next_cwd.to_path_buf())
            .await
            .unwrap_or_else(|_| {
                panic!("shell environment actor stopped while the session is running");
            });
    }

    pub(crate) async fn update_settings(
        &self,
        updates: SessionSettingsUpdate,
    ) -> ConstraintResult<()> {
        let mut state = self.state.lock().await;

        match self
            .apply_configuration_update(&state.session_configuration, &updates)
            .await
        {
            Ok(updated) => {
                let previous_cwd = state.session_configuration.cwd.clone();
                let next_cwd = updated.cwd.clone();
                let session_source = updated.session_source.clone();
                state.session_configuration = updated.clone();
                drop(state);

                self.permission_actor
                    .set_session_defaults(&updated)
                    .await
                    .unwrap_or_else(|_| {
                        panic!("permission actor stopped while the session is running");
                    });

                self.maybe_refresh_shell_environment_for_cwd(
                    &previous_cwd,
                    &next_cwd,
                    &session_source,
                )
                .await;

                if previous_cwd != next_cwd
                    && let Err(e) = self
                        .services
                        .mcp_registry
                        .notify_roots_changed(&next_cwd)
                        .await
                {
                    warn!("Failed to notify MCP servers of roots change: {e:#}");
                }

                Ok(())
            }
            Err(err) => {
                warn!("rejected session settings update: {err}");
                Err(err)
            }
        }
    }

    pub(crate) async fn get_config(&self) -> Arc<Config> {
        let state = self.state.lock().await;
        state
            .session_configuration
            .original_config_do_not_use
            .clone()
    }

    pub(crate) async fn set_dynamic_parent_effort(&self, enabled: bool) {
        let mut state = self.state.lock().await;
        let mut config = (*state.session_configuration.original_config_do_not_use).clone();
        config.dynamic_parent_effort = enabled;
        state.session_configuration.original_config_do_not_use = Arc::new(config);
    }

    pub(crate) async fn reload_user_config_layer(&self) {
        let chaos_home = {
            let state = self.state.lock().await;
            state.session_configuration.chaos_home.clone()
        };

        let snapshot = match crate::user_settings::snapshot(&chaos_home).await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                warn!("failed to reload database user settings: {err}");
                return;
            }
        };
        let installation = match crate::user_settings::installation_snapshot(&chaos_home).await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                warn!("failed to reload installation settings: {err}");
                return;
            }
        };

        let mut state = self.state.lock().await;
        let mut config = (*state.session_configuration.original_config_do_not_use).clone();
        config.config_layer_stack = config
            .config_layer_stack
            .with_database_settings(SettingsScope::Global, snapshot.revision, snapshot.settings)
            .with_database_settings(
                SettingsScope::Installation,
                installation.revision,
                installation.settings,
            );
        let reflex = config
            .config_layer_stack
            .effective_config()
            .get("reflex")
            .cloned()
            .map(toml::Value::try_into)
            .transpose();
        match reflex {
            Ok(reflex) => config.reflex = reflex.unwrap_or_default(),
            Err(_) => {
                warn!("failed to reload reflex settings");
                return;
            }
        }
        let enabled = config
            .config_layer_stack
            .effective_config()
            .get("reflex_enabled")
            .cloned()
            .map(toml::Value::try_into::<bool>)
            .transpose();
        match enabled {
            Ok(enabled) => config.reflex_enabled = enabled.unwrap_or(true),
            Err(_) => {
                warn!("failed to reload reflex enabled setting");
                return;
            }
        }
        state.session_configuration.original_config_do_not_use = Arc::new(config);
    }

    pub(crate) async fn reload_project_mcp_layer_and_refresh(
        self: &Arc<Self>,
        turn_context: &TurnContext,
    ) {
        let session = Arc::clone(self);
        let turn_context = turn_context.clone();
        self.services
            .mcp_refresh
            .run(async move {
                session
                    .reload_project_mcp_layer_and_refresh_now(&turn_context)
                    .await;
            })
            .await
            .unwrap_or_else(|_| {
                panic!("MCP refresh actor stopped while the session is running");
            });
    }

    async fn reload_project_mcp_layer_and_refresh_now(&self, turn_context: &TurnContext) {
        let (bootstrap, cwd, config_layer_stack) = {
            let state = self.state.lock().await;
            let session_config = &state.session_configuration;
            let config = &session_config.original_config_do_not_use;
            (
                config.resolved_bootstrap(),
                session_config.cwd.clone(),
                config.config_layer_stack.clone(),
            )
        };

        let layer = match crate::config_loader::reload_project_mcp_layer(
            &bootstrap,
            &cwd,
            &config_layer_stack,
        )
        .await
        {
            Ok(layer) => layer,
            Err(err) => {
                warn!("failed to reload project MCP config: {err}");
                return;
            }
        };

        let (mcp_servers, store_mode) = {
            let mut state = self.state.lock().await;
            let mut config = (*state.session_configuration.original_config_do_not_use).clone();
            config.config_layer_stack = config.config_layer_stack.with_project_mcp_layer(layer);
            if let Err(err) = config.reload_mcp_servers_from_layer_stack().await {
                warn!("failed to reload MCP servers from updated config stack: {err}");
                return;
            }
            let mcp_servers = config.mcp_servers.get().clone();
            let store_mode = config.mcp_oauth_credentials_store_mode;
            state.session_configuration.original_config_do_not_use = Arc::new(config);
            (mcp_servers, store_mode)
        };

        match self
            .refresh_mcp_servers_inner(turn_context, mcp_servers, store_mode)
            .await
        {
            Ok(event) => {
                // Clients watching `chaos://mcp` need to know the set of
                // servers changed, not just the model.
                self.send_event(turn_context, EventMsg::McpServersRefreshed(event))
                    .await;
            }
            Err(err) => {
                warn!("failed to refresh MCP servers from project configuration: {err:#}");
            }
        }
    }
}
