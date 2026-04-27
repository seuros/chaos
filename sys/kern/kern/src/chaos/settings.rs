use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::SubAgentSource;
use chaos_realpath::AbsolutePathBuf;
use chaos_sysctl::CONFIG_TOML_FILE;
use tracing::warn;

use super::Session;
use super::TurnContext;
use crate::config::Config;
use crate::config::ConstraintResult;

use crate::shell_snapshot::ShellSnapshot;

/// Convenience wrapper used by `new_turn_with_sub_id`.
pub(super) use super::SessionSettingsUpdate;

impl Session {
    pub(crate) async fn chaos_home(&self) -> PathBuf {
        let state = self.state.lock().await;
        state.session_configuration.chaos_home().clone()
    }

    /// Refresh the shell snapshot when the working directory changes, unless
    /// this session is a sub-agent process spawn (which inherits from parent).
    pub(super) fn maybe_refresh_shell_snapshot_for_cwd(
        &self,
        previous_cwd: &Path,
        next_cwd: &Path,
        chaos_home: &Path,
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

        ShellSnapshot::refresh_snapshot(
            chaos_home.to_path_buf(),
            self.conversation_id,
            next_cwd.to_path_buf(),
            self.services.user_shell.as_ref().clone(),
            self.services.shell_snapshot_tx.clone(),
            self.services.session_telemetry.clone(),
        );
    }

    pub(crate) async fn update_settings(
        &self,
        updates: SessionSettingsUpdate,
    ) -> ConstraintResult<()> {
        let mut state = self.state.lock().await;

        match state.session_configuration.apply(&updates) {
            Ok(updated) => {
                let previous_cwd = state.session_configuration.cwd.clone();
                let next_cwd = updated.cwd.clone();
                let chaos_home = updated.chaos_home.clone();
                let session_source = updated.session_source.clone();
                state.session_configuration = updated;
                drop(state);

                self.maybe_refresh_shell_snapshot_for_cwd(
                    &previous_cwd,
                    &next_cwd,
                    &chaos_home,
                    &session_source,
                );

                if previous_cwd != next_cwd
                    && let Err(e) = self
                        .services
                        .mcp_connection_manager
                        .read()
                        .await
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

    pub(crate) async fn reload_user_config_layer(&self) {
        let config_toml_path = {
            let state = self.state.lock().await;
            state
                .session_configuration
                .chaos_home
                .join(CONFIG_TOML_FILE)
        };

        let user_config = match std::fs::read_to_string(&config_toml_path) {
            Ok(contents) => match toml::from_str::<toml::Value>(&contents) {
                Ok(config) => config,
                Err(err) => {
                    warn!("failed to parse user config while reloading layer: {err}");
                    return;
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                toml::Value::Table(Default::default())
            }
            Err(err) => {
                warn!("failed to read user config while reloading layer: {err}");
                return;
            }
        };

        let config_toml_path = match AbsolutePathBuf::try_from(config_toml_path) {
            Ok(path) => path,
            Err(err) => {
                warn!("failed to resolve user config path while reloading layer: {err}");
                return;
            }
        };

        let mut state = self.state.lock().await;
        let mut config = (*state.session_configuration.original_config_do_not_use).clone();
        config.config_layer_stack = config
            .config_layer_stack
            .with_user_config(&config_toml_path, user_config);
        state.session_configuration.original_config_do_not_use = Arc::new(config);
    }

    pub(crate) async fn reload_project_mcp_layer_and_refresh(&self, turn_context: &TurnContext) {
        let project_mcp_json_path = {
            let state = self.state.lock().await;
            let session_config = &state.session_configuration;
            crate::config_loader::project_mcp_json_path_for_stack(
                &session_config.original_config_do_not_use.config_layer_stack,
                &session_config.cwd,
            )
        };

        let layer = match std::fs::read_to_string(&project_mcp_json_path) {
            Ok(contents) => match crate::config_loader::parse_project_mcp_json(&contents) {
                Ok(config) => {
                    let Ok(file) = AbsolutePathBuf::try_from(project_mcp_json_path.clone()) else {
                        warn!(
                            "failed to resolve project MCP path while \
                                 reloading layer: {}",
                            project_mcp_json_path.display()
                        );
                        return;
                    };
                    Some(crate::config_loader::ConfigLayerEntry::new(
                        chaos_ipc::api::ConfigLayerSource::ProjectMcp { file },
                        config,
                    ))
                }
                Err(err) => {
                    warn!(
                        "failed to parse project MCP config while \
                             reloading layer: {err}"
                    );
                    return;
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                warn!("failed to read project MCP config while reloading layer: {err}");
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

        self.refresh_mcp_servers_inner(turn_context, mcp_servers, store_mode)
            .await;
    }
}
