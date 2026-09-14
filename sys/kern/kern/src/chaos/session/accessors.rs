use std::collections::HashMap;
use std::sync::Arc;

use async_channel::Sender;
use chaos_dtrace::Hooks;
use chaos_ipc::config_types::CollaborationMode;
use chaos_ipc::protocol::Event;

use super::Session;

impl Session {
    pub(crate) fn get_tx_event(&self) -> Sender<Event> {
        self.tx_event.clone()
    }

    pub(crate) fn runtime_db(&self) -> Option<crate::runtime_db::RuntimeDbHandle> {
        self.services.runtime_db.clone()
    }

    pub(crate) async fn collaboration_mode(&self) -> CollaborationMode {
        let state = self.state.lock().await;
        state.session_configuration.collaboration_mode.clone()
    }

    pub(crate) fn hooks(&self) -> &Hooks {
        &self.services.hooks
    }

    pub(crate) fn user_shell(&self) -> Arc<crate::shell::Shell> {
        Arc::clone(&self.services.user_shell)
    }

    pub(crate) async fn take_pending_session_start_source(
        &self,
    ) -> Option<chaos_dtrace::SessionStartSource> {
        let mut state = self.state.lock().await;
        state.take_pending_session_start_source()
    }

    pub async fn dependency_env(&self) -> HashMap<String, String> {
        let state = self.state.lock().await;
        state.dependency_env()
    }

    /// Effective config to use for child spawns.
    ///
    /// Prefer the currently active turn's config when a task is running so
    /// minions inherit live turn overrides (approval/sandbox/cwd/model
    /// provider/etc.) rather than the session's init-time config snapshot.
    /// When no turn is active, rebuild from the latest `SessionConfiguration`
    /// so session-level updates are still reflected.
    pub(crate) async fn effective_config_for_spawn(&self) -> crate::config::Config {
        let active_config = {
            let active_turn = self.active_turn.lock().await;
            if let Some(active_turn) = active_turn.as_ref()
                && let Some((_, task)) = active_turn.tasks.first()
            {
                Some((*task.turn_context.config).clone())
            } else {
                None
            }
        };

        let state = self.state.lock().await;
        let mut config = active_config
            .unwrap_or_else(|| Self::build_per_turn_config(&state.session_configuration));
        config.mode_policy_override = Some(state.session_configuration.mode_policy.clone());
        config
    }

    /// The session's own `SessionSource`. Used to derive a
    /// `SubAgentSource::ProcessSpawn` for child spawns so depth
    /// and parent linkage are tagged correctly.
    pub(crate) async fn session_source(&self) -> chaos_ipc::protocol::SessionSource {
        let state = self.state.lock().await;
        state.session_configuration.session_source.clone()
    }
}

#[cfg(test)]
mod tests;
