use std::collections::HashMap;
use std::sync::Arc;

use async_channel::Sender;
use chaos_dtrace::Hooks;
use chaos_ipc::config_types::CollaborationMode;
use chaos_ipc::protocol::Event;

use crate::config::ManagedFeatures;
use crate::features::Feature;

use super::Session;

impl Session {
    pub(crate) fn get_tx_event(&self) -> Sender<Event> {
        self.tx_event.clone()
    }

    pub(crate) fn runtime_db(&self) -> Option<crate::runtime_db::RuntimeDbHandle> {
        self.services.runtime_db.clone()
    }

    pub fn enabled(&self, feature: Feature) -> bool {
        self.features.enabled(feature)
    }

    pub(crate) fn features(&self) -> ManagedFeatures {
        self.features.clone()
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

    /// Snapshot of the session's base `Config` as captured at session
    /// init. Callers that need to spawn a sub-agent clone this and
    /// overlay per-agent overrides (model / instructions / cwd)
    /// rather than reaching into `SessionConfiguration` directly.
    pub(crate) async fn base_config(&self) -> crate::config::Config {
        let state = self.state.lock().await;
        (*state.session_configuration.original_config_do_not_use).clone()
    }

    /// The session's own `SessionSource`. Used to derive a
    /// `SubAgentSource::ProcessSpawn` for child spawns so depth
    /// and parent linkage are tagged correctly.
    pub(crate) async fn session_source(&self) -> chaos_ipc::protocol::SessionSource {
        let state = self.state.lock().await;
        state.session_configuration.session_source.clone()
    }
}
